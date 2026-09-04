use std::{fs, path::PathBuf, process};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use rusqlite::Connection;
use sha2::{Digest as _, Sha256};

use crate::tools::{ToolOutput, ToolResult};

use super::{
    ActivationOutcome, CompletedAssistant, DefaultModelSelection, DispatchOutcome,
    MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PrepareOperationOutcome,
    ProviderUsage, RunModelSelection, RunOpenCodeService, RunState, SessionEventCursor,
    SessionEventPayload, SessionStore, SubagentModelSetting, TranscriptCursor, TranscriptEntry,
    TranscriptPageDirection,
};
const TEST_MODEL: &str = "muse-spark-1.2";

#[tokio::test(flavor = "current_thread")]
async fn transcript_windows_reach_both_edges_beyond_the_old_client_limit() {
    let root = TestRoot::new("long-transcript-window");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x71; 16]), None)
        .await
        .expect("session should be created");
    for index in 0_u64..513 {
        let mut request_id = [0_u8; 16];
        request_id[8..].copy_from_slice(&index.saturating_add(1).to_be_bytes());
        let command = format!("printf MESSAGE-{index:03}");
        let accepted = store
            .accept_local_command(
                MutationRequestId::from_bytes(request_id),
                session.id,
                command,
                false,
            )
            .await
            .expect("bounded fixture command should be accepted");
        assert!(
            store
                .activate_local_command(accepted.id)
                .await
                .expect("fixture command should activate")
        );
        store
            .complete_local_command(
                accepted.id,
                ToolResult::Ok {
                    output: ToolOutput::Bash {
                        exit_code: Some(0),
                        signal: None,
                        stdout: format!("MESSAGE-{index:03}"),
                        stderr: String::new(),
                    },
                },
            )
            .await
            .expect("fixture command should complete");
    }

    let latest = store
        .list_session_transcript_window(session.id, None, TranscriptPageDirection::Older, 1)
        .await
        .expect("latest edge should load");
    assert!(latest.older_cursor.is_some());
    assert!(latest.newer_cursor.is_none());
    assert!(matches!(
        &latest.entries[..],
        [TranscriptEntry::LocalCommand { command, .. }] if command == "printf MESSAGE-512"
    ));

    let oldest = store
        .list_session_transcript_window(session.id, None, TranscriptPageDirection::Newer, 1)
        .await
        .expect("oldest edge should load");
    assert!(oldest.older_cursor.is_none());
    assert!(oldest.newer_cursor.is_some());
    assert!(matches!(
        &oldest.entries[..],
        [TranscriptEntry::LocalCommand { command, .. }] if command == "printf MESSAGE-000"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_run_input_does_not_append_transcript_state() {
    let root = TestRoot::new("rejected-run-input");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x09; 16]), None)
        .await
        .expect("session should be created");
    let error = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x0a; 16]),
            session.id,
            "must not commit".to_owned(),
            model_selection(),
        )
        .await
        .expect_err("missing credential should reject input");
    assert!(matches!(error, PersistenceError::CredentialNotConfigured));
    let transcript = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("empty transcript should remain readable");
    assert!(transcript.entries.is_empty());
    assert!(transcript.next_cursor.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_working_directory_rejects_run_before_transcript_commit() {
    let root = TestRoot::new("unavailable-run-directory");
    let selected = TestRoot::new("selected-run-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x09; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    fs::remove_dir_all(selected.path()).expect("selected directory should be removed");

    assert!(matches!(
        store
            .accept_session_input(
                MutationRequestId::from_bytes([0x0a; 16]),
                session.id,
                "must not commit".to_owned(),
                model_selection(),
            )
            .await,
        Err(PersistenceError::WorkingDirectoryUnavailable)
    ));
    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should remain readable");
    assert!(page.entries.is_empty());
    assert!(page.runs.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn archived_sessions_reject_new_runs_before_transcript_commit() {
    let root = TestRoot::new("archived-run-input");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x0b; 16]), None)
        .await
        .expect("session should be created");
    store
        .set_session_archived(MutationRequestId::from_bytes([0x0c; 16]), session.id, true)
        .await
        .expect("session should archive");
    assert!(matches!(
        store
            .accept_session_input(
                MutationRequestId::from_bytes([0x0d; 16]),
                session.id,
                "must not commit".to_owned(),
                model_selection(),
            )
            .await,
        Err(PersistenceError::SessionArchived)
    ));
    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should remain readable");
    assert!(page.entries.is_empty());
    assert!(page.runs.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn deleting_an_archived_session_removes_file_backed_images_only_from_morons_state() {
    let root = TestRoot::new("delete-image-session");
    let selected = TestRoot::new("delete-image-selected");
    let sentinel = selected.path().join("sentinel");
    fs::write(&sentinel, "keep").expect("sentinel should be written");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x0e; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x44; 16]).expect("fixture image should normalize");
    let orphan_bytes = image.bytes.clone();
    let attachment = crate::persistence::PreparedImageAttachment {
        display_name: "picture.png".to_owned(),
        marker_start: 0,
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        digest: Sha256::digest(&image.bytes).into(),
        bytes: image.bytes,
    };
    let mut selection = model_selection();
    selection.supports_image_input = true;
    let accepted = store
        .accept_session_input_with_skills(
            MutationRequestId::from_bytes([0x0f; 16]),
            session.id,
            "[picture.png]".to_owned(),
            selection,
            crate::skills::RunSkillContext::default(),
            vec![attachment],
        )
        .await
        .expect("image-bearing run should be accepted");
    let attachment_session_directory = fs::read_dir(root.path().join("attachments"))
        .expect("attachment root should be readable")
        .next()
        .expect("attachment directory should exist")
        .expect("attachment directory should be readable")
        .path();
    let orphan = attachment_session_directory.join("11111111111111111111111111111111.image");
    fs::write(&orphan, orphan_bytes).expect("orphan fixture should be written");
    #[cfg(unix)]
    fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600))
        .expect("orphan fixture should be private");
    store
        .finish_run_stopped(accepted.run.id, None)
        .await
        .expect("run should stop before archiving");
    store
        .set_session_archived(MutationRequestId::from_bytes([0x10; 16]), session.id, true)
        .await
        .expect("session should archive");
    store
        .delete_session(MutationRequestId::from_bytes([0x11; 16]), session.id)
        .await
        .expect("session should delete");
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment root should remain readable")
            .count(),
        0
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
    drop(store);
    let reopened = SessionStore::open_at(root.path()).expect("deleted store should reopen");
    assert!(
        reopened
            .get_session(session.id)
            .await
            .expect("session query should succeed")
            .is_none()
    );
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "keep");
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_and_used_models_determine_the_durable_global_default() {
    let root = TestRoot::new("default-model");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    assert!(
        store
            .default_model()
            .await
            .expect("empty default should be readable")
            .is_none()
    );

    let request_id = MutationRequestId::from_bytes([0x40; 16]);
    let explicit = DefaultModelSelection {
        service: RunOpenCodeService::Go,
        model_id: "grok-4.6".to_owned(),
    };
    assert_eq!(
        store
            .set_default_model(request_id, explicit.clone())
            .await
            .expect("default should be selected"),
        explicit
    );
    assert_eq!(
        store
            .set_default_model(request_id, explicit.clone())
            .await
            .expect("an exact selection retry should resolve"),
        explicit
    );
    let conflict = store
        .set_default_model(
            request_id,
            DefaultModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: TEST_MODEL.to_owned(),
            },
        )
        .await
        .expect_err("a conflicting selection retry should fail");
    assert!(matches!(conflict, PersistenceError::RequestConflict));
    assert_eq!(
        store
            .default_model()
            .await
            .expect("selected default should be readable"),
        Some(explicit)
    );

    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x41; 16]), None)
        .await
        .expect("session should be created");
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0x42; 16]),
            session.id,
            "use the newer model".to_owned(),
            model_selection(),
        )
        .await
        .expect("run should be accepted");
    let used = DefaultModelSelection {
        service: RunOpenCodeService::Zen,
        model_id: TEST_MODEL.to_owned(),
    };
    assert_eq!(
        store
            .default_model()
            .await
            .expect("last used model should be readable"),
        Some(used.clone())
    );

    drop(store);
    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    assert_eq!(
        reopened
            .default_model()
            .await
            .expect("default should survive restart"),
        Some(used)
    );
    drop(reopened);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE default_model_selections SET operation_fingerprint = zeroblob(32)",
            [],
        )
        .expect("default fingerprint should be corruptible for test");
    drop(connection);
    let corrupted = SessionStore::open_at(root.path());
    assert!(matches!(
        corrupted,
        Err(PersistenceError::InvalidState { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn subagent_model_setting_is_global_idempotent_and_durable() {
    let root = TestRoot::new("subagent-model-setting");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    assert_eq!(
        store
            .subagent_model_setting()
            .await
            .expect("initial setting should load"),
        SubagentModelSetting::InheritParent {}
    );

    let request_id = MutationRequestId::from_bytes([0x43; 16]);
    let selected = SubagentModelSetting::OpenCode {
        service: RunOpenCodeService::Go,
        model_id: "glm-5.3-flash".to_owned(),
    };
    for _ in 0..2 {
        assert_eq!(
            store
                .set_subagent_model_setting(request_id, selected.clone())
                .await
                .expect("setting should be selected idempotently"),
            selected
        );
    }
    assert!(matches!(
        store
            .set_subagent_model_setting(request_id, SubagentModelSetting::InheritParent {})
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    assert_eq!(
        store
            .subagent_model_setting()
            .await
            .expect("selected setting should load"),
        selected
    );

    let inherit = SubagentModelSetting::InheritParent {};
    store
        .set_subagent_model_setting(MutationRequestId::from_bytes([0x44; 16]), inherit.clone())
        .await
        .expect("inherit setting should be restored");
    drop(store);
    let reopened = SessionStore::open_at(root.path()).expect("setting should survive restart");
    assert_eq!(
        reopened
            .subagent_model_setting()
            .await
            .expect("reopened setting should load"),
        inherit
    );
    drop(reopened);

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE subagent_model_selections SET operation_fingerprint = zeroblob(32)",
            [],
        )
        .expect("setting fingerprint should be corruptible for test");
    drop(connection);
    assert!(matches!(
        SessionStore::open_at(root.path()),
        Err(PersistenceError::InvalidState { .. })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn run_input_is_atomic_idempotent_and_session_serialized() {
    let root = TestRoot::new("run-acceptance");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x11; 16]), None)
        .await
        .expect("session should be created");
    let request_id = MutationRequestId::from_bytes([0x12; 16]);
    let accepted = store
        .accept_session_input(
            request_id,
            session.id,
            "hello durable run".to_owned(),
            model_selection(),
        )
        .await
        .expect("input should be accepted");

    assert!(accepted.newly_accepted);
    assert_eq!(accepted.run.state, RunState::Accepted);
    assert_eq!(accepted.run.credential_generation, 1);
    assert_eq!(
        accepted.run.tool_catalog_version,
        crate::tools::TOOL_CATALOG_VERSION
    );
    assert_eq!(
        accepted.run.tool_limits_version,
        crate::tools::TOOL_LIMITS_VERSION
    );
    let retry = store
        .find_session_input_retry(
            request_id,
            session.id,
            "hello durable run",
            RunOpenCodeService::Zen,
            TEST_MODEL,
        )
        .await
        .expect("retry should resolve")
        .expect("retry should exist");
    assert!(!retry.newly_accepted);
    assert_eq!(retry.run.id, accepted.run.id);
    assert_eq!(retry.user_message_id, accepted.user_message_id);

    let conflict = store
        .find_session_input_retry(
            request_id,
            session.id,
            "different input",
            RunOpenCodeService::Zen,
            TEST_MODEL,
        )
        .await
        .expect_err("conflicting retry should fail");
    assert!(matches!(conflict, PersistenceError::RequestConflict));

    let busy = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x13; 16]),
            session.id,
            "must not queue".to_owned(),
            model_selection(),
        )
        .await
        .expect_err("a session with a run should be busy");
    assert!(matches!(
        busy,
        PersistenceError::SessionBusy { active_run_id } if active_run_id == accepted.run.id
    ));

    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should be readable");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.active_run_id, Some(accepted.run.id));
    assert!(matches!(
        &page.runs[..],
        [run] if run.id == accepted.run.id && run.state == RunState::Accepted
    ));
    assert!(matches!(
        &page.entries[0],
        TranscriptEntry::UserMessage { id, run_id, text, .. }
            if *id == accepted.user_message_id
                && *run_id == accepted.run.id
                && text == "hello durable run"
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn accepted_run_binds_active_skill_instructions_and_catalog_metadata() {
    let root = TestRoot::new("run-skill-context");
    let selected = TestRoot::new("run-skill-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xb1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let skills = crate::skills::RunSkillContext {
        skills: vec![crate::skills::SkillSnapshot {
            name: "release-helper".to_owned(),
            description: "Helps prepare a release when explicitly requested.".to_owned(),
            skill_file: selected
                .path()
                .join(".agents/skills/release-helper/SKILL.md")
                .to_string_lossy()
                .into_owned(),
            source: crate::skills::SkillSource::Project,
            active: true,
            instructions: Some(
                "---\nname: release-helper\ndescription: Helps prepare a release when explicitly requested.\n---\nRun checks first.\n"
                    .to_owned(),
            ),
        }],
    };
    let accepted = store
        .accept_session_input_with_skills(
            MutationRequestId::from_bytes([0xb2; 16]),
            session.id,
            "@release-helper prepare this release".to_owned(),
            model_selection(),
            skills.clone(),
            Vec::new(),
        )
        .await
        .expect("skill-bearing run should be accepted");
    assert_eq!(
        accepted.run.context_policy_version,
        crate::persistence::run_types::CONTEXT_POLICY_VERSION
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("skill-bearing context should load");
    assert_eq!(context.skills, skills);
    assert!(
        context
            .skills
            .developer_text()
            .is_some_and(|text| text.contains("Run checks first."))
    );
    drop(store);

    let reopened = SessionStore::open_at(root.path()).expect("session store should reopen");
    assert_eq!(
        reopened
            .load_run_context(accepted.run.id)
            .await
            .expect("durable skill context should reload")
            .skills,
        skills
    );
    drop(reopened);
    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE run_skill_snapshots SET skill_name = 'INVALID' WHERE run_id = ?1",
            [&accepted.run.id.as_bytes()[..]],
        )
        .expect("skill snapshot should be corrupted");
    drop(connection);
    let error = match SessionStore::open_at(root.path()) {
        Ok(_) => panic!("invalid skill snapshot should fail closed"),
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn image_attachments_are_fingerprint_bound_file_backed_and_durable() {
    let root = TestRoot::new("run-image-context");
    let selected = TestRoot::new("run-image-directory");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xc1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x44; 16]).expect("fixture image should normalize");
    let attachment = crate::persistence::PreparedImageAttachment {
        display_name: "picture.png".to_owned(),
        marker_start: 5,
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        digest: Sha256::digest(&image.bytes).into(),
        bytes: image.bytes,
    };
    let mut selection = model_selection();
    selection.supports_image_input = true;
    let request_id = MutationRequestId::from_bytes([0xc2; 16]);
    let accepted = store
        .accept_session_input_with_skills(
            request_id,
            session.id,
            "look [picture.png]".to_owned(),
            selection.clone(),
            crate::skills::RunSkillContext::default(),
            vec![attachment.clone()],
        )
        .await
        .expect("image-bearing run should be accepted");
    let retry = store
        .accept_session_input_with_skills(
            request_id,
            session.id,
            "look [picture.png]".to_owned(),
            selection,
            crate::skills::RunSkillContext::default(),
            vec![attachment.clone()],
        )
        .await
        .expect("exact image retry should resolve");
    assert_eq!(retry.run.id, accepted.run.id);
    assert!(!retry.newly_accepted);

    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("image transcript should load");
    let [TranscriptEntry::UserMessage { attachments, .. }] = &page.entries[..] else {
        panic!("user message should remain canonical");
    };
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].display_name, "picture.png");
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("image context should load");
    assert_eq!(context.attachment_data.len(), 1);
    assert_eq!(
        context
            .attachment_data
            .get(&attachments[0].id)
            .map(Vec::as_slice),
        Some(attachment.bytes.as_slice())
    );
    assert!(matches!(
        store
            .accept_session_input_with_skills(
                MutationRequestId::from_bytes([0xc3; 16]),
                session.id,
                "continue without vision".to_owned(),
                model_selection(),
                crate::skills::RunSkillContext::default(),
                Vec::new(),
            )
            .await,
        Err(PersistenceError::ImageInputUnsupported)
    ));
    assert_eq!(
        fs::read_dir(root.path().join("attachments"))
            .expect("attachment directory should be readable")
            .count(),
        1
    );
    drop(store);
    let attachment_session_directory = fs::read_dir(root.path().join("attachments"))
        .expect("attachment root should be readable")
        .next()
        .expect("attachment session directory should exist")
        .expect("attachment session entry should load")
        .path();
    let orphan = attachment_session_directory.join("11111111111111111111111111111111.image");
    fs::write(&orphan, &attachment.bytes).expect("orphan fixture should be written");
    #[cfg(unix)]
    fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600))
        .expect("orphan should be owner-only");
    let reopened =
        SessionStore::open_at(root.path()).expect("image attachment should survive reopen");
    assert!(!orphan.exists());
    drop(reopened);
    let durable_file = fs::read_dir(attachment_session_directory)
        .expect("attachment directory should remain readable")
        .next()
        .expect("durable attachment should exist")
        .expect("durable attachment entry should load")
        .path();
    #[cfg(unix)]
    {
        let directory_metadata = fs::symlink_metadata(
            durable_file
                .parent()
                .expect("durable attachment should have a parent"),
        )
        .expect("attachment session directory metadata should load");
        assert_eq!(directory_metadata.mode() & 0o777, 0o700);
        let metadata =
            fs::symlink_metadata(&durable_file).expect("durable attachment metadata should load");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    }
    fs::remove_file(durable_file).expect("durable attachment should be removed for corruption");
    let error = match SessionStore::open_at(root.path()) {
        Ok(_) => panic!("missing durable attachment should fail closed"),
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::Io(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_session_input_accepts_one_run_without_queueing() {
    let root = TestRoot::new("concurrent-run-acceptance");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x61; 16]), None)
        .await
        .expect("session should be created");
    let first = store.accept_session_input(
        MutationRequestId::from_bytes([0x62; 16]),
        session.id,
        "first concurrent input".to_owned(),
        model_selection(),
    );
    let second = store.accept_session_input(
        MutationRequestId::from_bytes([0x63; 16]),
        session.id,
        "second concurrent input".to_owned(),
        model_selection(),
    );
    let (first, second) = tokio::join!(first, second);
    let outcomes = [first, second];
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(PersistenceError::SessionBusy { .. })))
            .count(),
        1
    );
    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should remain readable");
    assert_eq!(page.entries.len(), 1);
    assert!(page.next_cursor.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn complete_provider_outcome_commits_assistant_and_terminal_run() {
    let root = TestRoot::new("run-completion");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x21; 16]), None)
        .await
        .expect("session should be created");
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            "answer this".to_owned(),
            model_selection(),
        )
        .await
        .expect("input should be accepted");
    assert_eq!(
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate"),
        ActivationOutcome::Active
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("run context should load");
    assert_eq!(context.entries.len(), 1);
    let operation_id = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("provider operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation_id) => operation_id,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    assert_eq!(
        store
            .mark_provider_dispatched(accepted.run.id, operation_id)
            .await
            .expect("provider operation should dispatch"),
        DispatchOutcome::Dispatched
    );
    let completed = store
        .complete_run_success(
            accepted.run.id,
            operation_id,
            CompletedAssistant {
                text: "durable answer".to_owned(),
                refusal: false,
                provider_response_id: "resp_test".to_owned(),
                usage: ProviderUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 4,
                    reasoning_output_tokens: 0,
                    total_tokens: 14,
                },
            },
        )
        .await
        .expect("provider outcome should complete");
    assert_eq!(completed.state, RunState::Succeeded);
    let retry = store
        .find_session_input_retry(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            "answer this",
            RunOpenCodeService::Zen,
            TEST_MODEL,
        )
        .await
        .expect("terminal input retry should resolve")
        .expect("terminal input retry should exist");
    assert_eq!(retry.run.id, accepted.run.id);
    assert_eq!(retry.run.state, RunState::Accepted);

    let first = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("first transcript page should load");
    assert_eq!(first.session.id, session.id);
    assert_eq!(first.active_run_id, None);
    assert!(matches!(
        &first.runs[..],
        [run] if run.id == accepted.run.id && run.state == RunState::Succeeded
    ));
    let events = store
        .read_session_events(session.id, SessionEventCursor::new(session.id, 0), 100)
        .await
        .expect("session events should replay");
    assert_eq!(events.events.len(), 5);
    assert!(matches!(
        &events.events[0].payload,
        SessionEventPayload::TranscriptEntry(TranscriptEntry::UserMessage { run_id, .. })
            if *run_id == accepted.run.id
    ));
    assert!(matches!(
        &events.events[1].payload,
        SessionEventPayload::RunChanged(run) if run.state == RunState::Accepted
    ));
    assert!(matches!(
        &events.events[2].payload,
        SessionEventPayload::RunChanged(run) if run.state == RunState::Active
    ));
    assert!(matches!(
        &events.events[3].payload,
        SessionEventPayload::TranscriptEntry(TranscriptEntry::AssistantMessage { run_id, .. })
            if *run_id == accepted.run.id
    ));
    assert!(matches!(
        &events.events[4].payload,
        SessionEventPayload::RunChanged(run) if run.state == RunState::Succeeded
    ));
    assert_eq!(events.high_water, first.event_cursor);
    let continuation = first
        .next_cursor
        .expect("completed transcript should have another page");
    let forged_cursor = TranscriptCursor::new(
        session.id,
        continuation.snapshot_entry_sequence(),
        0,
        continuation.after_entry_sequence(),
    );
    let forged_snapshot = store
        .list_session_transcript(session.id, Some(forged_cursor), 1)
        .await
        .expect_err("inconsistent transcript high waters should fail");
    assert!(matches!(
        forged_snapshot,
        PersistenceError::InvalidInput { .. }
    ));
    let other_session = store
        .create_session(MutationRequestId::from_bytes([0x24; 16]), None)
        .await
        .expect("second session should be created");
    let cross_session = store
        .list_session_transcript(other_session.id, first.next_cursor, 1)
        .await
        .expect_err("cross-session transcript cursor should fail");
    assert!(matches!(
        cross_session,
        PersistenceError::InvalidInput { .. }
    ));
    let cross_session_events = store
        .read_session_events(other_session.id, first.event_cursor, 1)
        .await
        .expect_err("cross-session event cursor should fail");
    assert!(matches!(
        cross_session_events,
        PersistenceError::InvalidInput { .. }
    ));
    let second = store
        .list_session_transcript(session.id, first.next_cursor, 1)
        .await
        .expect("second transcript page should load");
    assert!(first.next_cursor.is_some());
    assert!(second.next_cursor.is_none());
    assert_eq!(second.event_cursor, first.event_cursor);
    assert_eq!(second.active_run_id, None);
    assert!(matches!(
        &second.entries[0],
        TranscriptEntry::AssistantMessage { run_id, text, .. }
            if *run_id == accepted.run.id && text == "durable answer"
    ));

    let latest = store
        .list_session_transcript_window(session.id, None, TranscriptPageDirection::Older, 1)
        .await
        .expect("latest transcript window should load");
    assert!(latest.newer_cursor.is_none());
    let older_cursor = latest
        .older_cursor
        .expect("latest entry should have older history");
    assert!(matches!(
        &latest.entries[..],
        [TranscriptEntry::AssistantMessage { text, .. }] if text == "durable answer"
    ));
    let older = store
        .list_session_transcript_window(
            session.id,
            Some(older_cursor),
            TranscriptPageDirection::Older,
            1,
        )
        .await
        .expect("older transcript window should load");
    assert!(older.older_cursor.is_none());
    let newer_cursor = older
        .newer_cursor
        .expect("oldest entry should link back to newer history");
    assert!(matches!(
        &older.entries[..],
        [TranscriptEntry::UserMessage { text, .. }] if text == "answer this"
    ));
    let newer = store
        .list_session_transcript_window(
            session.id,
            Some(newer_cursor),
            TranscriptPageDirection::Newer,
            1,
        )
        .await
        .expect("newer transcript window should load");
    assert!(newer.newer_cursor.is_none());
    assert!(newer.older_cursor.is_some());
    assert!(matches!(
        &newer.entries[..],
        [TranscriptEntry::AssistantMessage { text, .. }] if text == "durable answer"
    ));

    let next = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x23; 16]),
            session.id,
            "continue deterministically".to_owned(),
            model_selection(),
        )
        .await
        .expect("a new run should be accepted after success");
    store
        .activate_run(next.run.id)
        .await
        .expect("next run should activate");
    let fixed_snapshot = store
        .list_session_transcript(session.id, first.next_cursor, 1)
        .await
        .expect("fixed transcript snapshot should remain readable");
    assert!(fixed_snapshot.next_cursor.is_none());
    assert_eq!(fixed_snapshot.event_cursor, first.event_cursor);
    assert_eq!(fixed_snapshot.active_run_id, None);
    assert!(matches!(
        &fixed_snapshot.runs[..],
        [run] if run.id == accepted.run.id && run.state == RunState::Succeeded
    ));
    assert!(matches!(
        &fixed_snapshot.entries[..],
        [TranscriptEntry::AssistantMessage { run_id, .. }] if *run_id == accepted.run.id
    ));
    let context = store
        .load_run_context(next.run.id)
        .await
        .expect("next run context should load");
    assert_eq!(context.entries.len(), 3);
    assert!(matches!(
        &context.entries[..],
        [
            TranscriptEntry::UserMessage { .. },
            TranscriptEntry::AssistantMessage { .. },
            TranscriptEntry::UserMessage { run_id, .. },
        ] if *run_id == next.run.id
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_is_exact_durable_and_stops_before_dispatch() {
    let root = TestRoot::new("run-cancellation");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    configure_credential(&store).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0x31; 16]), None)
        .await
        .expect("session should be created");
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x32; 16]),
            session.id,
            "cancel this".to_owned(),
            model_selection(),
        )
        .await
        .expect("input should be accepted");
    store
        .activate_run(accepted.run.id)
        .await
        .expect("run should activate");
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("run context should load");
    let operation_id = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("provider operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation_id) => operation_id,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    let cancellation_id = MutationRequestId::from_bytes([0x33; 16]);
    let cancellation = store
        .cancel_run(cancellation_id, session.id, accepted.run.id)
        .await
        .expect("cancellation intent should commit");
    assert!(cancellation.intent_applied);
    assert_eq!(cancellation.state, RunState::Active);
    assert!(cancellation.cancellation_requested);
    let retry = store
        .cancel_run(cancellation_id, session.id, accepted.run.id)
        .await
        .expect("cancellation retry should resolve");
    assert_eq!(retry, cancellation);
    assert_eq!(
        store
            .mark_provider_dispatched(accepted.run.id, operation_id)
            .await
            .expect("dispatch boundary should observe cancellation"),
        DispatchOutcome::Cancelled
    );
    let run = store
        .get_run(session.id, accepted.run.id)
        .await
        .expect("run query should succeed")
        .expect("run should exist");
    assert_eq!(run.state, RunState::Cancelled);
    let events = store
        .read_session_events(session.id, SessionEventCursor::new(session.id, 0), 100)
        .await
        .expect("cancellation events should replay");
    assert!(matches!(
        &events.events[3].payload,
        SessionEventPayload::RunChanged(run)
            if run.id == accepted.run.id
                && run.state == RunState::Active
                && run.cancellation_requested
    ));
    assert!(matches!(
        &events.events[4].payload,
        SessionEventPayload::RunChanged(run)
            if run.id == accepted.run.id && run.state == RunState::Cancelled
    ));
    let terminal = store
        .cancel_run(
            MutationRequestId::from_bytes([0x34; 16]),
            session.id,
            accepted.run.id,
        )
        .await
        .expect("terminal cancellation should return terminal state");
    assert_eq!(terminal.state, RunState::Cancelled);
    assert!(terminal.cancellation_requested);
    assert!(!terminal.intent_applied);
}

#[tokio::test(flavor = "current_thread")]
async fn startup_never_replays_a_dispatched_subagent_batch() {
    let root = TestRoot::new("subagent-run-recovery");
    let session_id;
    let run_id;
    {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x3a; 16]), None)
            .await
            .expect("session should be created");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x3b; 16]),
                session.id,
                "delegate work".to_owned(),
                model_selection(),
            )
            .await
            .expect("input should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        let context = store
            .load_run_context(accepted.run.id)
            .await
            .expect("run context should load");
        let provider_operation_id = match store
            .prepare_provider_operation(
                accepted.run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .await
            .expect("provider operation should prepare")
        {
            PrepareOperationOutcome::Prepared(operation_id) => operation_id,
            other => panic!("unexpected preparation outcome: {other:?}"),
        };
        assert_eq!(
            store
                .mark_provider_dispatched(accepted.run.id, provider_operation_id)
                .await
                .expect("provider operation should dispatch"),
            DispatchOutcome::Dispatched
        );
        let committed = store
            .complete_provider_tool_turn(
                accepted.run.id,
                provider_operation_id,
                super::CompletedToolTurn {
                    provider_response_id: "resp_subagent_recovery".to_owned(),
                    usage: ProviderUsage {
                        input_tokens: 10,
                        cached_input_tokens: 0,
                        cache_write_input_tokens: 0,
                        output_tokens: 2,
                        reasoning_output_tokens: 0,
                        total_tokens: 12,
                    },
                    commentary: None,
                    calls: vec![crate::tools::ValidatedProviderCall {
                        provider_call_id: "provider_task_recovery".to_owned(),
                        input: crate::tools::ToolInput::Task {
                            context: "shared context".to_owned(),
                            tasks: vec![crate::tools::SubagentTask {
                                name: Some("worker".to_owned()),
                                task: "inspect state".to_owned(),
                            }],
                        },
                    }],
                },
            )
            .await
            .expect("task call should commit");
        let call = &committed.calls[0];
        store
            .prepare_tool_operation(accepted.run.id, call.call_id, call.operation_id, None)
            .await
            .expect("task operation should prepare");
        store
            .mark_tool_dispatched(accepted.run.id, call.call_id, call.operation_id)
            .await
            .expect("task operation should dispatch");
        session_id = session.id;
        run_id = accepted.run.id;
    }

    let store = SessionStore::open_at(root.path()).expect("recovery should complete");
    let run = store
        .get_run(session_id, run_id)
        .await
        .expect("run query should succeed")
        .expect("run should remain");
    assert_eq!(run.state, RunState::Uncertain);
    let mut cursor = None;
    let mut entries = Vec::new();
    loop {
        let page = store
            .list_session_transcript(session_id, cursor, 1)
            .await
            .expect("transcript should page");
        entries.extend(page.entries);
        let Some(next) = page.next_cursor else { break };
        cursor = Some(next);
    }
    assert!(matches!(
        entries.last(),
        Some(TranscriptEntry::ToolResult {
            tool: crate::tools::ToolKind::Task,
            result: crate::tools::ToolResult::Error {
                error: crate::tools::ToolErrorKind::Uncertain,
                ..
            },
            ..
        })
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn startup_interrupts_nonterminal_runs_without_redispatch() {
    let root = TestRoot::new("run-recovery");
    let run_id;
    let session_id;
    {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x41; 16]), None)
            .await
            .expect("session should be created");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x42; 16]),
                session.id,
                "recover me".to_owned(),
                model_selection(),
            )
            .await
            .expect("input should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        let context = store
            .load_run_context(accepted.run.id)
            .await
            .expect("run context should load");
        let operation_id = match store
            .prepare_provider_operation(
                accepted.run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .await
            .expect("provider operation should prepare")
        {
            PrepareOperationOutcome::Prepared(operation_id) => operation_id,
            other => panic!("unexpected preparation outcome: {other:?}"),
        };
        store
            .mark_provider_dispatched(accepted.run.id, operation_id)
            .await
            .expect("provider operation should be marked dispatched");
        run_id = accepted.run.id;
        session_id = session.id;
    }

    let reopened = SessionStore::open_at(root.path()).expect("session store should recover");
    let recovered = reopened
        .get_run(session_id, run_id)
        .await
        .expect("recovered run query should succeed")
        .expect("recovered run should exist");
    assert_eq!(recovered.state, RunState::Interrupted);
    reopened
        .accept_session_input(
            MutationRequestId::from_bytes([0x43; 16]),
            session_id,
            "new run after recovery".to_owned(),
            model_selection(),
        )
        .await
        .expect("interrupted provider usage should not block new input");
}

#[tokio::test(flavor = "current_thread")]
async fn run_projections_rebuild_and_canonical_corruption_fails_closed() {
    let root = TestRoot::new("run-projection-repair");
    let run_id;
    let session_id;
    {
        let store = SessionStore::open_at(root.path()).expect("session store should open");
        configure_credential(&store).await;
        let session = store
            .create_session(MutationRequestId::from_bytes([0x51; 16]), None)
            .await
            .expect("session should be created");
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([0x52; 16]),
                session.id,
                "repair projections".to_owned(),
                model_selection(),
            )
            .await
            .expect("input should be accepted");
        store
            .activate_run(accepted.run.id)
            .await
            .expect("run should activate");
        run_id = accepted.run.id;
        session_id = session.id;
    }

    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open for damage");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF")
        .expect("test should disable foreign-key enforcement");
    connection
        .execute(
            "UPDATE session_run_states SET active_run_id = zeroblob(16)",
            [],
        )
        .expect("session state projection should accept test-only foreign-key damage");
    connection
        .execute("DELETE FROM runs", [])
        .expect("run projection should be removable");
    connection
        .execute("DELETE FROM delivery_events WHERE event_kind != 1", [])
        .expect("delivery projections should be removable");
    drop(connection);

    let repaired = SessionStore::open_at(root.path()).expect("projections should rebuild");
    let recovered = repaired
        .get_run(session_id, run_id)
        .await
        .expect("run query should succeed")
        .expect("run should remain present");
    assert_eq!(recovered.state, RunState::Interrupted);
    drop(repaired);

    let connection = Connection::open(&database_path).expect("database should open for corruption");
    connection
        .execute(
            "UPDATE run_input_requests SET operation_fingerprint = zeroblob(32)",
            [],
        )
        .expect("canonical fingerprint should be corruptible for test");
    drop(connection);
    let error = match SessionStore::open_at(root.path()) {
        Ok(store) => {
            drop(store);
            panic!("canonical corruption should fail");
        }
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

async fn configure_credential(store: &SessionStore) -> OpenCodeCredentialStatus {
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x01; 16]),
            0,
            b"not-a-real-run-test-key".to_vec(),
        )
        .await
        .expect("credential should be configured")
}

fn model_selection() -> RunModelSelection {
    RunModelSelection {
        service: RunOpenCodeService::Zen,
        model_id: TEST_MODEL.to_owned(),
        protocol_revision: 1,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        supports_tool_calls: true,
        supports_image_input: false,
    }
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("test randomness should be available");
        let encoded = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path =
            std::env::temp_dir().join(format!("morons-run-{label}-{}-{encoded}", process::id()));
        fs::create_dir(&path).expect("test root should be created");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("test root should be owner-only");
        #[cfg(windows)]
        fence_windows::harden_private_directory(&path)
            .expect("Windows test root should be hardened");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
