use super::hardening::{fixture, selection};
use super::*;

fn database(root: &TestRoot) -> rusqlite::Connection {
    rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap()
}

fn set_last_usage(connection: &rusqlite::Connection) {
    connection.execute(
        "UPDATE provider_operation_facts SET input_tokens = 12000, cached_input_tokens = 8000,
             cache_write_input_tokens = 1000, output_tokens = 10, total_tokens = 12010
         WHERE fact_sequence = (SELECT MAX(fact_sequence) FROM provider_operation_facts WHERE fact_kind = 3)", [],
    ).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn observed_prefix_defers_soft_compaction_but_never_weakens_hard_limits() {
    for (new_bytes, must_compact) in [(5_000, false), (35_000, true)] {
        let (root, _selected, store, session) = fixture("usage-pressure").await;
        append_completed_context_run(&store, session, 1, "OLD", 56_000).await;
        append_completed_context_run(&store, session, 2, "RECENT", 0).await;
        set_last_usage(&database(&root));
        let accepted = store
            .accept_session_input(
                PersistenceMutationRequestId::from_bytes([3; 16]),
                session,
                "x".repeat(new_bytes),
                selection(),
            )
            .await
            .unwrap();
        let status = store
            .session_context_status(session, selection())
            .await
            .unwrap();
        assert!(status.estimate_uses_provider_usage);
        assert!(status.estimated_input_tokens < status.compaction_threshold_tokens);
        assert!(status.conservative_input_tokens > status.compaction_threshold_tokens);
        let usage = status.latest_provider_usage.unwrap();
        assert_eq!(usage.cached_input_tokens, 8_000);
        assert_eq!(usage.cache_write_input_tokens, 1_000);
        assert_eq!(usage.input_tokens, 12_000);
        let context = store.load_run_context(accepted.run.id).await.unwrap();
        assert_eq!(context.compaction_plan.is_some(), must_compact);
        if !must_compact {
            assert!(context.estimated_input_tokens > status.estimated_input_tokens);
            build_provider_request(&context, None).unwrap();
        }
        store
            .finish_run_stopped(accepted.run.id, None)
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn observations_are_model_protocol_and_skill_bound_and_exclude_hidden_commands() {
    let (root, _selected, store, session) = fixture("usage-binding").await;
    append_completed_context_run(&store, session, 1, "SOURCE", 0).await;
    set_last_usage(&database(&root));
    let baseline = store
        .session_context_status(session, selection())
        .await
        .unwrap();
    assert!(baseline.estimate_uses_provider_usage);
    assert_eq!(
        baseline.estimated_input_tokens,
        12_000 + 10 + "SOURCE_ASSISTANT ".len() as u32 + 16 + 8_192
    );
    assert!(
        baseline
            .latest_provider_usage
            .unwrap()
            .elapsed_milliseconds
            .is_some()
    );
    for kind in 0..3 {
        let mut other = selection();
        match kind {
            0 => other.service = RunOpenCodeService::Go,
            1 => other.model_id = "gpt-5-nano".to_owned(),
            _ => other.protocol_revision = 2,
        }
        let status = store.session_context_status(session, other).await.unwrap();
        assert!(!status.estimate_uses_provider_usage);
        assert!(status.latest_provider_usage.is_none());
    }
    let command = store
        .accept_local_command(
            PersistenceMutationRequestId::from_bytes([2; 16]),
            session,
            "HIDDEN_COMMAND".to_owned(),
            false,
        )
        .await
        .unwrap();
    store.activate_local_command(command.id).await.unwrap();
    store
        .complete_local_command(
            command.id,
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "HIDDEN_OUTPUT".repeat(2_000),
                    stderr: String::new(),
                },
            },
        )
        .await
        .unwrap();
    let status = store
        .session_context_status(session, selection())
        .await
        .unwrap();
    assert_eq!(
        status.estimated_input_tokens,
        baseline.estimated_input_tokens
    );
    assert_eq!(
        status.conservative_input_tokens,
        baseline.conservative_input_tokens
    );
    let skills = crate::skills::RunSkillContext {
        skills: vec![crate::skills::SkillSnapshot {
            name: "changed".to_owned(),
            description: "Changed workflow".to_owned(),
            skill_file: "/fixture/SKILL.md".to_owned(),
            source: crate::skills::SkillSource::Project,
            active: true,
            instructions: Some(
                "---\nname: changed\ndescription: Changed workflow\n---\n\nUse the new workflow.\n"
                    .to_owned(),
            ),
        }],
    };
    let accepted = store
        .accept_session_input_with_skills(
            PersistenceMutationRequestId::from_bytes([3; 16]),
            session,
            "next".to_owned(),
            selection(),
            skills,
            Vec::new(),
        )
        .await
        .unwrap();
    let status = store
        .session_context_status(session, selection())
        .await
        .unwrap();
    assert!(!status.estimate_uses_provider_usage);
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn new_checkpoint_invalidates_old_usage_and_records_only_completed_compactions() {
    let (_root, _selected, store, session) = fixture("usage-checkpoint").await;
    append_completed_context_run(&store, session, 1, "OLD", 0).await;
    let accepted = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([2; 16]),
            session,
            "/compact".to_owned(),
            selection(),
        )
        .await
        .unwrap();
    store.activate_run(accepted.run.id).await.unwrap();
    assert!(
        store
            .session_context_status(session, selection())
            .await
            .unwrap()
            .estimate_uses_provider_usage
    );
    let plan = store
        .load_run_context(accepted.run.id)
        .await
        .unwrap()
        .compaction_plan
        .unwrap();
    let operation = store
        .prepare_auto_compaction(accepted.run.id, &plan)
        .await
        .unwrap();
    store
        .mark_compaction_dispatched(accepted.run.id, operation)
        .await
        .unwrap();
    assert_eq!(
        store
            .session_context_status(session, selection())
            .await
            .unwrap()
            .completed_compactions,
        0
    );
    store
        .complete_compaction(
            accepted.run.id,
            operation,
            RunOpenCodeService::Zen,
            selection().model_id,
            "SOURCE SUMMARY".to_owned(),
        )
        .await
        .unwrap();
    let status = store
        .session_context_status(session, selection())
        .await
        .unwrap();
    assert!(!status.estimate_uses_provider_usage);
    assert_eq!(status.completed_compactions, 1);
    assert!(status.last_compaction_milliseconds.is_some());
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn old_usage_outside_the_bounded_run_window_falls_back_without_history_scanning() {
    let (_root, _selected, store, session) = fixture("usage-window").await;
    append_completed_context_run(&store, session, 1, "SOURCE", 0).await;
    for id in 2..=9 {
        let accepted = store
            .accept_session_input(
                PersistenceMutationRequestId::from_bytes([id; 16]),
                session,
                "no successful inference".to_owned(),
                selection(),
            )
            .await
            .unwrap();
        store
            .finish_run_stopped(accepted.run.id, None)
            .await
            .unwrap();
    }
    let status = store
        .session_context_status(session, selection())
        .await
        .unwrap();
    assert!(!status.estimate_uses_provider_usage);
    assert!(status.latest_provider_usage.is_none());
    assert_eq!(
        status.estimated_input_tokens,
        status.conservative_input_tokens
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_committed_usage_is_never_used_as_a_context_estimate() {
    let (root, _selected, store, session) = fixture("usage-corrupt").await;
    append_completed_context_run(&store, session, 1, "SOURCE", 0).await;
    database(&root)
        .execute(
            "UPDATE provider_operation_facts SET cached_input_tokens = 100 WHERE fact_kind = 3",
            [],
        )
        .unwrap();
    assert!(
        store
            .session_context_status(session, selection())
            .await
            .is_err()
    );
}

async fn image_fixture() -> (
    TestRoot,
    TestRoot,
    SessionStore,
    crate::persistence::SessionId,
    crate::persistence::RunId,
    RunModelSelection,
) {
    use sha2::{Digest as _, Sha256};
    let (root, selected, store, session) = fixture("status-images").await;
    let image = morons_image::normalize_rgba(2, 2, vec![0x44; 16]).unwrap();
    let mut text = String::new();
    let mut attachments = Vec::new();
    for index in 0..4 {
        let name = format!("image-{index}.png");
        let start = text.len() as u32;
        text.push_str(&format!("[{name}] "));
        attachments.push(crate::persistence::PreparedImageAttachment {
            display_name: name,
            marker_start: start,
            media_type: image.media_type,
            width: image.width,
            height: image.height,
            digest: Sha256::digest(&image.bytes).into(),
            bytes: image.bytes.clone(),
        });
    }
    let mut model = selection();
    model.model_id = "gpt-5-nano".to_owned();
    model.supports_image_input = true;
    let accepted = store
        .accept_session_input_with_skills(
            PersistenceMutationRequestId::from_bytes([1; 16]),
            session,
            text,
            model.clone(),
            crate::skills::RunSkillContext::default(),
            attachments,
        )
        .await
        .unwrap();
    (root, selected, store, session, accepted.run.id, model)
}

#[tokio::test(flavor = "current_thread")]
async fn context_status_does_not_read_images_but_execution_still_validates_them() {
    let (root, _selected, store, session, run, model) = image_fixture().await;
    assert_eq!(
        store
            .load_run_context(run)
            .await
            .unwrap()
            .attachment_data
            .len(),
        4
    );
    let directory = fs::read_dir(root.path().join("attachments"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let file = fs::read_dir(directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::remove_file(file).unwrap();
    let status = store.session_context_status(session, model).await.unwrap();
    assert!(status.conservative_input_tokens > 32_768);
    assert!(store.load_run_context(run).await.is_err());
    store.finish_run_stopped(run, None).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "manual local timing probe; no network, real credentials or timing assertions"]
async fn measure_metadata_context_status() {
    use std::{hint::black_box, time::Instant};
    let (_root, _selected, store, session, run, model) = image_fixture().await;
    for _ in 0..5 {
        black_box(store.load_run_context(run).await.unwrap());
        black_box(
            store
                .session_context_status(session, model.clone())
                .await
                .unwrap(),
        );
    }
    let start = Instant::now();
    for _ in 0..200 {
        black_box(store.load_run_context(run).await.unwrap());
    }
    let full = start.elapsed();
    let start = Instant::now();
    for _ in 0..200 {
        black_box(
            store
                .session_context_status(session, model.clone())
                .await
                .unwrap(),
        );
    }
    eprintln!(
        "200 observations, 4 images: full_context={full:?}, metadata_status={:?}",
        start.elapsed()
    );
    store.finish_run_stopped(run, None).await.unwrap();
}
