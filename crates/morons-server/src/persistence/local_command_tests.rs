use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use morons_protocol::{
    ApplicationRequest, ApplicationResponse, LocalCommandStatus as ProtocolCommandStatus,
    MutationRequestId as ProtocolMutationRequestId, SessionId as ProtocolSessionId,
};
use tokio::time::{self, Duration};

use super::{
    LocalCommandStatus, MutationRequestId, RunModelSelection, RunOpenCodeService, SessionStore,
    TranscriptEntry,
};
use crate::application::{ApplicationOutcome, ServerApplication};
use crate::tools::{ToolOutput, ToolResult};

#[tokio::test(flavor = "current_thread")]
async fn local_commands_are_idempotent_durable_and_context_visibility_is_explicit() {
    let state = TestRoot::new("local-command-state");
    let selected = TestRoot::new("local-command-selected");
    let store = SessionStore::open_for_test(state.path()).expect("session store should open");
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x21; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let accepted = store
        .accept_local_command(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            "printf visible".to_owned(),
            true,
        )
        .await
        .expect("command should be accepted");
    let retry = store
        .accept_local_command(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            "printf visible".to_owned(),
            true,
        )
        .await
        .expect("exact retry should resolve");
    assert_eq!(retry.id, accepted.id);
    assert!(store.activate_local_command(accepted.id).await.unwrap());
    store
        .complete_local_command(
            accepted.id,
            ToolResult::Ok {
                output: ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "visible".to_owned(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .expect("command result should commit");

    let hidden = store
        .accept_local_command(
            MutationRequestId::from_bytes([0x23; 16]),
            session.id,
            "printf hidden".to_owned(),
            false,
        )
        .await
        .expect("context-excluded command should be accepted");
    assert!(store.activate_local_command(hidden.id).await.unwrap());
    store
        .complete_local_command(
            hidden.id,
            ToolResult::Ok {
                output: ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "hidden".to_owned(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .expect("context-excluded command should complete");
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x24; 16]),
            0,
            b"not-a-real-local-command-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    let run = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x25; 16]),
            session.id,
            "what happened?".to_owned(),
            RunModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: "muse-spark-1.2".to_owned(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 8_192,
                supports_tool_calls: true,
                supports_image_input: false,
            },
        )
        .await
        .expect("run should be accepted");
    let context = store
        .load_run_context(run.run.id)
        .await
        .expect("run context should load");
    assert_eq!(
        context
            .entries
            .iter()
            .filter(|entry| matches!(entry, TranscriptEntry::LocalCommand { .. }))
            .count(),
        1
    );
    assert!(context.entries.iter().any(|entry| matches!(
        entry,
        TranscriptEntry::LocalCommand {
            context_visible: true,
            stdout,
            ..
        } if stdout == "visible"
    )));

    let page = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("transcript should load");
    assert_eq!(page.active_command_id, None);
    assert!(matches!(
        &page.entries[0],
        TranscriptEntry::LocalCommand {
            command_id,
            context_visible: true,
            status: LocalCommandStatus::Succeeded,
            stdout,
            ..
        } if *command_id == accepted.id && stdout == "visible"
    ));
    drop(store);
    SessionStore::open_for_test(state.path()).expect("durable command should reopen");
}

#[tokio::test(flavor = "current_thread")]
async fn application_command_mode_executes_in_the_selected_directory_and_publishes_history() {
    let state = TestRoot::new("application-command-state");
    let selected = TestRoot::new("application-command-selected");
    let store = SessionStore::open_for_test(state.path()).expect("session store should open");
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x31; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let session_id = ProtocolSessionId::from_bytes(*session.id.as_bytes());
    let application = ServerApplication::from_session_store(store);
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::ExecuteLocalCommand {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x32; 16]),
            session_id,
            command: "printf command-mode > command.txt; printf captured".to_owned(),
            context_visible: false,
        })
        .await
        .expect("local command should be accepted");
    assert!(matches!(
        accepted,
        ApplicationOutcome::Response(ApplicationResponse::LocalCommandAccepted { .. })
    ));

    let entry = time::timeout(Duration::from_secs(10), async {
        loop {
            let outcome = application
                .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
                    session_id,
                    cursor: None,
                    limit: 1,
                })
                .await
                .expect("transcript should load");
            let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
                entries,
                ..
            }) = outcome
            else {
                panic!("transcript should be returned");
            };
            if let Some(entry) = entries.into_iter().next() {
                break entry;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("command should finish");
    assert!(matches!(
        entry,
        morons_protocol::TranscriptEntry::LocalCommand {
            context_visible: false,
            status: ProtocolCommandStatus::Succeeded,
            ref stdout,
            ..
        } if stdout == "captured"
    ));
    assert_eq!(
        fs::read_to_string(selected.path().join("command.txt")).unwrap(),
        "command-mode"
    );
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn archiving_stops_active_commands_without_touching_the_selected_directory() {
    let state = TestRoot::new("archive-command-state");
    let selected = TestRoot::new("archive-command-selected");
    let sentinel = selected.path().join("sentinel");
    fs::write(&sentinel, "keep").expect("sentinel should be written");
    let store = SessionStore::open_for_test(state.path()).expect("session store should open");
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x61; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let session_id = ProtocolSessionId::from_bytes(*session.id.as_bytes());
    let application = ServerApplication::from_session_store(store);
    application
        .execute_for_local_owner(ApplicationRequest::ExecuteLocalCommand {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x62; 16]),
            session_id,
            command: "(sleep 2; printf leaked > leaked) & wait".to_owned(),
            context_visible: true,
        })
        .await
        .expect("local command should be accepted");
    let outcome = application
        .execute_for_local_owner(ApplicationRequest::SetSessionArchived {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x63; 16]),
            session_id,
            archived: true,
        })
        .await
        .expect("archive should stop active work and complete");
    assert!(matches!(
        outcome,
        ApplicationOutcome::Response(ApplicationResponse::SessionArchiveChanged {
            session: morons_protocol::SessionSummary { archived: true, .. }
        })
    ));
    time::sleep(Duration::from_millis(300)).await;
    assert!(!selected.path().join("leaked").exists());
    assert_eq!(fs::read_to_string(sentinel).unwrap(), "keep");
    let rejected = application
        .execute_for_local_owner(ApplicationRequest::ExecuteLocalCommand {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x64; 16]),
            session_id,
            command: "printf should-not-run".to_owned(),
            context_visible: true,
        })
        .await;
    assert!(matches!(
        rejected,
        Err(morons_protocol::ApplicationError::SessionArchived)
    ));
    application.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn local_command_cancellation_stops_descendants_and_commits_interruption() {
    let state = TestRoot::new("command-cancel-state");
    let selected = TestRoot::new("command-cancel-selected");
    let store = SessionStore::open_for_test(state.path()).expect("session store should open");
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0x41; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("session should be created");
    let session_id = ProtocolSessionId::from_bytes(*session.id.as_bytes());
    let application = ServerApplication::from_session_store(store);
    let accepted = application
        .execute_for_local_owner(ApplicationRequest::ExecuteLocalCommand {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x42; 16]),
            session_id,
            command: "(sleep 2; printf leaked > leaked) & wait".to_owned(),
            context_visible: true,
        })
        .await
        .expect("local command should be accepted");
    let ApplicationOutcome::Response(ApplicationResponse::LocalCommandAccepted { command_id }) =
        accepted
    else {
        panic!("local command acceptance should be returned");
    };
    let cancelled = application
        .execute_for_local_owner(ApplicationRequest::CancelLocalCommand {
            mutation_request_id: ProtocolMutationRequestId::from_bytes([0x43; 16]),
            session_id,
            command_id,
        })
        .await
        .expect("local command cancellation should resolve");
    assert!(matches!(
        cancelled,
        ApplicationOutcome::Response(ApplicationResponse::LocalCommandCancellationResolved {
            cancellation_requested: true,
            ..
        })
    ));
    let entry = time::timeout(Duration::from_secs(10), async {
        loop {
            let outcome = application
                .execute_for_local_owner(ApplicationRequest::ListSessionTranscript {
                    session_id,
                    cursor: None,
                    limit: 1,
                })
                .await
                .expect("transcript should load");
            let ApplicationOutcome::Response(ApplicationResponse::SessionTranscriptListed {
                entries,
                ..
            }) = outcome
            else {
                panic!("transcript should be returned");
            };
            if let Some(entry) = entries.into_iter().next() {
                break entry;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelled command should finish");
    assert!(matches!(
        entry,
        morons_protocol::TranscriptEntry::LocalCommand {
            status: ProtocolCommandStatus::Interrupted,
            ..
        }
    ));
    time::sleep(Duration::from_millis(300)).await;
    assert!(!selected.path().join("leaked").exists());
    application.shutdown().await;
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should be available")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "morons-command-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test root should be created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("test root should be private");
        }
        #[cfg(windows)]
        fence_windows::harden_private_directory(&path).expect("test root should be private");
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
