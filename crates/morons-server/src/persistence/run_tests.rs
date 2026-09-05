use std::{fs, path::PathBuf, process};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use rusqlite::Connection;
use sha2::{Digest as _, Sha256};

use crate::tools::{ToolOutput, ToolResult};

use super::{
    ActivationOutcome, CompletedAssistant, CompletedToolTurn, DefaultModelSelection,
    DispatchOutcome, MutationRequestId, OpenCodeCredentialStatus, PersistenceError,
    PrepareOperationOutcome, ProviderUsage, RunModelSelection, RunOpenCodeService, RunState,
    SessionEventCursor, SessionEventPayload, SessionStore, SubagentModelSetting, TranscriptCursor,
    TranscriptEntry, TranscriptPageDirection,
};
const TEST_MODEL: &str = "muse-spark-1.2";

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

mod admission;
mod attachments_and_history;
mod lifecycle;
mod selection;
