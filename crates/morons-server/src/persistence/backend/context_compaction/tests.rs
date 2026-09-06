use super::*;
use crate::persistence::{
    ContextCheckpointId, MutationRequestId, RunId, RunModelSelection, RunOpenCodeService,
    SessionStore, tests::TestRoot,
};

pub(in crate::persistence::backend) async fn fixture(
    label: &str,
) -> (TestRoot, TestRoot, SessionStore, SessionId) {
    let root = TestRoot::new(label);
    let selected = TestRoot::new(&format!("{label}-directory"));
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0xe0; 16]),
            0,
            b"not-a-real-compaction-source-key".to_vec(),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xe1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap();
    (root, selected, store, session.id)
}

pub(in crate::persistence::backend) async fn append_stopped(
    store: &SessionStore,
    session_id: SessionId,
    request: u8,
    text: String,
) -> RunId {
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([request; 16]),
            session_id,
            text,
            RunModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
                supports_image_input: false,
            },
        )
        .await
        .unwrap();
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .unwrap();
    accepted.run.id
}

#[test]
fn excerpts_are_utf8_bounded_and_disclose_loss() {
    let text = "🐸".repeat(10_000);
    let excerpt = bounded_excerpt(&text, MAX_ENTRY_EXCERPT_BYTES);
    assert!(excerpt.len() <= MAX_ENTRY_EXCERPT_BYTES);
    assert!(excerpt.contains("Source excerpt truncated"));
    assert!(excerpt.starts_with('🐸') && excerpt.ends_with('🐸'));
}

#[tokio::test(flavor = "current_thread")]
async fn fixed_prefix_projection_survives_later_input_without_authorizing_dispatch() {
    let (root, _selected, store, session) = fixture("fixed-prefix").await;
    append_stopped(&store, session, 1, "FIRST".to_owned()).await;
    append_stopped(&store, session, 2, "SECOND".to_owned()).await;
    let triggering_run = append_stopped(&store, session, 3, "RETAINED".to_owned()).await;
    drop(store);
    let mut backend = Backend::open(root.path()).unwrap();
    let before = backend
        .project_compaction_prefix(session, None, 2, Some("  preserve the goal  "))
        .unwrap();
    assert_eq!(before.source, "USER:\nFIRST\n\nUSER:\nSECOND\n\n");
    assert_eq!(before.user_guidance.as_deref(), Some("preserve the goal"));
    assert!(matches!(
        backend.prepare_auto_compaction(triggering_run, &before),
        Err(PersistenceError::InvalidState {
            reason: "only an active run can prepare automatic compaction"
        })
    ));
    let effects: i64 = backend
        .connection
        .query_row(
            "SELECT (SELECT COUNT(*) FROM compaction_operations) + (SELECT COUNT(*) FROM context_checkpoints)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(effects, 0);
    drop(backend);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    append_stopped(&store, session, 4, "LATER_INPUT".to_owned()).await;
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    let after = backend
        .project_compaction_prefix(session, None, 2, Some("  preserve the goal  "))
        .unwrap();
    assert_eq!(before.source, after.source);
    assert_eq!(before.source_digest, after.source_digest);
    assert_eq!(
        before.source_entry_high_water,
        after.source_entry_high_water
    );
    assert_eq!(before.estimated_input_tokens, after.estimated_input_tokens);
    assert_eq!(before.user_guidance, after.user_guidance);
    assert_eq!(after.parent_checkpoint_id, None);
    assert_eq!(after.parent_summary, None);
}

#[tokio::test(flavor = "current_thread")]
async fn projection_separates_parent_summary_and_hashes_but_excludes_hidden_commands() {
    let (root, _selected, store, session) = fixture("prefix-parent").await;
    append_stopped(&store, session, 1, "OLD".to_owned()).await;
    let hidden = store
        .accept_local_command(
            MutationRequestId::from_bytes([2; 16]),
            session,
            "EXCLUDED_COMMAND".to_owned(),
            false,
        )
        .await
        .unwrap();
    assert!(store.activate_local_command(hidden.id).await.unwrap());
    store
        .complete_local_command(
            hidden.id,
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "EXCLUDED_OUTPUT".to_owned(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .unwrap();
    append_stopped(&store, session, 3, "NEW".to_owned()).await;
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    let parent = ContextCheckpoint {
        id: ContextCheckpointId::from_bytes([4; 16]),
        source_entry_high_water: 1,
        summary: "PARENT".to_owned(),
        estimated_summary_tokens: 22,
    };
    let plan = backend
        .project_compaction_prefix(session, Some(&parent), 3, Some("  "))
        .unwrap();
    assert_eq!(plan.source, "USER:\nNEW\n\n");
    assert_eq!(plan.parent_checkpoint_id, Some(parent.id));
    assert_eq!(plan.parent_summary, Some(parent.summary.clone()));
    assert_eq!(plan.user_guidance, None);
    assert_eq!(
        plan.source_digest,
        backend.context_digest_through(session, 3).unwrap()
    );
    let uncheckpointed = backend
        .project_compaction_prefix(session, None, 3, None)
        .unwrap();
    assert_eq!(plan.source_digest, uncheckpointed.source_digest);
    assert!(uncheckpointed.source.contains("OLD"));
    assert!(!uncheckpointed.source.contains("EXCLUDED"));
    let hidden_only = backend
        .project_compaction_prefix(session, Some(&parent), 2, None)
        .unwrap();
    assert_eq!(
        hidden_only.source,
        "[No context-visible source text in this prefix.]\n"
    );
    for invalid_cut in [0, 1, 4] {
        assert!(
            backend
                .project_compaction_prefix(session, Some(&parent), invalid_cut, None)
                .is_err()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn aggregate_projection_discloses_omissions_and_binds_the_full_canonical_prefix() {
    let (root, _selected, store, session) = fixture("prefix-excerpts").await;
    for index in 1..=9 {
        append_stopped(
            &store,
            session,
            index,
            format!("BULKY_{index} {}", "x".repeat(MAX_ENTRY_EXCERPT_BYTES)),
        )
        .await;
    }
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    let plan = backend
        .project_compaction_prefix(session, None, 9, Some(&"g".repeat(MAX_GUIDANCE_BYTES * 2)))
        .unwrap();
    assert!(plan.source.starts_with(OMITTED));
    assert!(plan.source.len() <= MAX_SOURCE_BYTES + OMITTED.len());
    assert!(!plan.source.contains("BULKY_1"));
    assert!(plan.source.contains("BULKY_9"));
    assert!(plan.source.contains("Source excerpt truncated"));
    assert!(plan.user_guidance.as_ref().unwrap().len() <= MAX_GUIDANCE_BYTES);
    assert_eq!(plan.estimated_input_tokens as usize, plan.source.len() + 32);
    assert_eq!(
        plan.source_digest,
        backend.context_digest_through(session, 9).unwrap()
    );
    let first: String = backend
        .connection
        .query_row(
            "SELECT text FROM session_entries WHERE session_id = ?1 AND entry_sequence = 1",
            [&session.as_bytes()[..]],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        first,
        format!("BULKY_1 {}", "x".repeat(MAX_ENTRY_EXCERPT_BYTES))
    );
}

#[tokio::test(flavor = "current_thread")]
async fn prefix_selection_retains_recent_turns_until_hard_tail_guards_require_a_later_cut() {
    let (root, _selected, store, session) = fixture("prefix-cut").await;
    for index in 1..=5 {
        append_stopped(&store, session, index, "x".repeat(1_000)).await;
    }
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    for cut in [2, 3, 4] {
        let maximum = backend
            .context_budget(session, cut, 5)
            .unwrap()
            .tokens(MAX_COMPACTION_SUMMARY_BYTES) as u32;
        assert_eq!(
            backend
                .select_compaction_prefix(session, 0, 5, 5, maximum, 0)
                .unwrap(),
            Some(cut)
        );
        assert_eq!(
            backend
                .select_compaction_prefix(session, 0, 5, 5, maximum, 1)
                .unwrap(),
            (cut < 4).then_some(cut + 1)
        );
    }
    for (covered, protected, through) in [(4, 5, 5), (0, 1, 5), (0, 5, 4), (u64::MAX, 5, 5)] {
        assert!(
            backend
                .select_compaction_prefix(session, covered, protected, through, 96_000, 0)
                .unwrap()
                .is_none()
        );
    }
}
