use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use morons_cli::{ApplicationClient, ApplicationClientError};
use morons_protocol::{
    ApplicationError, ApplicationEvent, ApplicationRequest,
    MutationRequestId as ProtocolMutationRequestId, OpenCodeService,
    SessionCatalogEventCursor as ProtocolSessionCatalogEventCursor,
    SessionEventCursor as ProtocolSessionEventCursor, SessionId as ProtocolSessionId,
    SessionListCursor as ProtocolSessionListCursor,
};
use rusqlite::{Connection, config::DbConfig, params};

use crate::{
    ConnectionError,
    application::{ApplicationOutcome, ServerApplication},
    handle_local_owner_requests,
};

use super::{
    MutationRequestId, PersistenceError, RunModelSelection, RunOpenCodeService,
    SessionCatalogEventCursor, SessionCatalogEventKind, SessionId, SessionListCursor, SessionStore,
    database,
    paths::{StoragePaths, encode_hex},
    types::create_session_fingerprint,
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

const SESSION_SUBSCRIPTION_TEST_TIMEOUT: Duration = if cfg!(windows) {
    Duration::from_secs(60)
} else {
    Duration::from_secs(30)
};
static TEST_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn session_store_open_error(root: &TestRoot, message: &str) -> PersistenceError {
    match SessionStore::open_at(root.path()) {
        Ok(store) => {
            drop(store);
            panic!("{message}");
        }
        Err(error) => error,
    }
}

fn pragma_integer(connection: &Connection, pragma: &'static str) -> i64 {
    connection
        .query_row(pragma, [], |row| row.get(0))
        .expect("pragma should return an integer")
}

#[cfg(unix)]
fn assert_mode(path: &Path, expected: u32) {
    let metadata = fs::symlink_metadata(path).expect("path metadata should be readable");
    assert_eq!(metadata.mode() & 0o777, expected);
    assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
}

fn protocol_session_event_cursor(
    session_id: ProtocolSessionId,
    sequence: u64,
) -> ProtocolSessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    ProtocolSessionEventCursor::from_bytes(bytes)
}

pub(super) struct TestRoot(PathBuf);

impl TestRoot {
    pub(super) fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be after Unix epoch")
            .as_nanos();
        let sequence = TEST_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "morons-persistence-{label}-{}-{timestamp}-{sequence}",
            std::process::id()
        ));

        #[cfg(unix)]
        {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(&path)
                .expect("private test root should be created");
        }
        #[cfg(not(unix))]
        fs::create_dir(&path).expect("test root should be created");
        #[cfg(windows)]
        fence_windows::harden_private_directory(&path)
            .expect("Windows test root should be hardened");

        Self(path)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Ok(metadata) = fs::symlink_metadata(self.path()) {
            let _ = fs::set_permissions(self.path(), fs::Permissions::from_mode(0o700));
            if metadata.file_type().is_symlink() {
                let _ = fs::remove_file(self.path());
                return;
            }
        }
        let _ = fs::remove_dir_all(self.path());
    }
}

mod integrity;
mod migrations;
mod recovery;
mod server_stop;
mod sessions;
mod subscriptions;
