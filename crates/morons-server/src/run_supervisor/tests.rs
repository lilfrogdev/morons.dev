use std::{fs, path::PathBuf, process, sync::Arc, time::Duration};

use morons_cli::ApplicationClient;

use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest, ApplicationResponse, MutationRequestId,
    OpenCodeService, RunId, RunState, SessionId, SubagentModelSetting,
};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
    time,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::{
    NormalizedTurn, RunSupervisor, build_provider_request, completed_assistant,
    enforce_image_capability, normalize_provider_turn,
};
use crate::{
    application::{ApplicationOutcome, ServerApplication, events::SessionEventHub},
    handle_local_owner_requests,
    persistence::{
        ActivationOutcome, MAX_TRANSCRIPT_TEXT_BYTES,
        MutationRequestId as PersistenceMutationRequestId, PrepareOperationOutcome,
        ProviderOperationFailureState, RunFailureKind, RunModelSelection, RunOpenCodeService,
        RunState as PersistenceRunState, SessionStore,
    },
    provider::{
        OpenCodeProvider, ProviderAssistantMessage, ProviderError, ProviderMessagePhase,
        ProviderOutcome, ProviderOutputItem, ProviderToolCall, ProviderUsage,
    },
};

mod hardening;
mod observations;
mod providers;
use providers::*;

const TERMINAL_RUN_TEST_TIMEOUT: Duration = if cfg!(windows) {
    Duration::from_secs(45)
} else {
    Duration::from_secs(15)
};

fn request_setting(request: &ApplicationRequest) -> SubagentModelSetting {
    match request {
        ApplicationRequest::SetSubagentModelSetting { setting, .. } => setting.clone(),
        _ => panic!("request should contain a subagent setting"),
    }
}

async fn append_completed_context_run(
    store: &SessionStore,
    session_id: crate::persistence::SessionId,
    request_byte: u8,
    marker: &str,
    assistant_padding: usize,
) {
    let accepted = store
        .accept_session_input(
            PersistenceMutationRequestId::from_bytes([request_byte; 16]),
            session_id,
            marker.to_owned(),
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
        .expect("context fixture run should be accepted");
    assert_eq!(
        store
            .activate_run(accepted.run.id)
            .await
            .expect("fixture run should activate"),
        ActivationOutcome::Active
    );
    let context = store
        .load_run_context(accepted.run.id)
        .await
        .expect("fixture context should load");
    let operation = match store
        .prepare_provider_operation(
            accepted.run.id,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .expect("fixture operation should prepare")
    {
        PrepareOperationOutcome::Prepared(operation) => operation,
        other => panic!("unexpected preparation outcome: {other:?}"),
    };
    assert!(matches!(
        store
            .mark_provider_dispatched(accepted.run.id, operation)
            .await
            .expect("fixture operation should dispatch"),
        crate::persistence::DispatchOutcome::Dispatched
    ));
    store
        .complete_run_success(
            accepted.run.id,
            operation,
            crate::persistence::CompletedAssistant {
                text: format!("{marker}_ASSISTANT {}", "x".repeat(assistant_padding)),
                refusal: false,
                provider_response_id: format!("resp_{marker}"),
                usage: crate::persistence::ProviderUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 10,
                    reasoning_output_tokens: 0,
                    total_tokens: 20,
                },
            },
        )
        .await
        .expect("fixture run should complete");
}

async fn wait_for_terminal(
    application: &ServerApplication,
    session_id: SessionId,
    run_id: RunId,
) -> RunState {
    time::timeout(TERMINAL_RUN_TEST_TIMEOUT, async {
        loop {
            let outcome = application
                .execute_for_local_owner(ApplicationRequest::GetRun { session_id, run_id })
                .await
                .expect("run query should succeed");
            let ApplicationOutcome::Response(ApplicationResponse::RunFound { run }) = outcome
            else {
                panic!("run query should return a run");
            };
            if run.state.is_terminal() {
                return run.state;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run should terminate")
}

fn request_body(request: &str) -> serde_json::Value {
    let (_, body) = request
        .split_once("\r\n\r\n")
        .expect("HTTP request headers should terminate");
    serde_json::from_str(body).expect("provider request body should be valid JSON")
}

fn request_header(request: &str, name: &str) -> String {
    let prefix = format!("{}:", name.to_ascii_lowercase());
    request
        .lines()
        .find_map(|line| {
            let lowercase = line.to_ascii_lowercase();
            lowercase
                .strip_prefix(&prefix)
                .map(|_| line[prefix.len()..].trim().to_owned())
        })
        .unwrap_or_else(|| panic!("request should contain {name}"))
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let bytes = stream.read(&mut chunk).await.expect("request should read");
        assert_ne!(bytes, 0, "request ended before headers");
        received.extend_from_slice(&chunk[..bytes]);
        assert!(received.len() <= 5 * 1024 * 1024);
        if let Some(position) = received.windows(4).position(|value| value == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&received[..header_end]).expect("headers should be UTF-8");
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    assert!(
        content_length <= 16 * 1024 * 1024,
        "fixture request body exceeds the provider limit"
    );
    while received.len() - header_end < content_length {
        let mut chunk = [0_u8; 4096];
        let bytes = stream.read(&mut chunk).await.expect("body should read");
        assert_ne!(bytes, 0, "request ended before body");
        received.extend_from_slice(&chunk[..bytes]);
    }
    received
}

fn write_test_skill(root: &std::path::Path, name: &str, description: &str, body: &str) {
    let directory = root.join(".agents/skills").join(name);
    fs::create_dir_all(&directory).expect("skill directory should be created");
    fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    )
    .expect("skill should be written");
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
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
        let path = std::env::temp_dir().join(format!(
            "morons-supervisor-{label}-{}-{encoded}",
            process::id()
        ));
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

mod compaction;
mod context;
mod lifecycle;
mod selection;
mod subagents;
mod tools;
mod validation;
