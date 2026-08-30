use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use morons_cli::{ApplicationClient, ApplicationClientError};
use morons_protocol::{
    ApplicationError, MutationRequestId as ProtocolMutationRequestId, OpenCodeApiKey,
    OpenCodeCredentialStatus as ProtocolOpenCodeCredentialStatus,
};
use rusqlite::{Connection, params};

use crate::{application::ServerApplication, handle_local_owner_requests};

use super::{
    MutationRequestId, OpenCodeCredentialStatus, PersistenceError, SessionStore,
    credentials::{CredentialStore, StoredOpenCodeApiKey},
    paths::create_private_file,
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

static TEST_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[tokio::test(flavor = "current_thread")]
async fn open_code_credentials_are_idempotent_versioned_and_durable() {
    const FIRST_KEY: &[u8] = b"not-a-real-first-key";
    const RETRY_KEY: &[u8] = b"not-a-real-retry-key";

    let root = TestRoot::new("credential-roundtrip");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    assert_eq!(
        store
            .open_code_credential_status()
            .await
            .expect("credential status should be readable"),
        OpenCodeCredentialStatus {
            configured: false,
            generation: 0,
        }
    );

    let set_request = MutationRequestId::from_bytes([0xa1; 16]);
    let configured = store
        .set_open_code_credential(set_request, 0, FIRST_KEY.to_vec())
        .await
        .expect("credential should be configured");
    assert_eq!(
        configured,
        OpenCodeCredentialStatus {
            configured: true,
            generation: 1,
        }
    );
    let retry = store
        .set_open_code_credential(set_request, 0, RETRY_KEY.to_vec())
        .await
        .expect("a completed mutation retry should return its prior result");
    assert_eq!(retry, configured);
    let credential_bytes = fs::read(root.path().join("credentials").join("opencode.state"))
        .expect("credential state should be readable for exact-retry verification");
    assert!(
        credential_bytes
            .windows(FIRST_KEY.len())
            .any(|window| window == FIRST_KEY)
    );
    assert!(
        !credential_bytes
            .windows(RETRY_KEY.len())
            .any(|window| window == RETRY_KEY)
    );

    let conflict = store
        .remove_open_code_credential(set_request, 1)
        .await
        .expect_err("cross-operation request reuse should fail");
    assert!(matches!(conflict, PersistenceError::RequestConflict));
    let stale = store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0xa2; 16]),
            0,
            RETRY_KEY.to_vec(),
        )
        .await
        .expect_err("a stale credential generation should fail");
    assert!(matches!(
        stale,
        PersistenceError::CredentialGenerationConflict
    ));
    let cross_resource = store
        .create_session(set_request, None)
        .await
        .expect_err("credential request identity cannot be reused for session creation");
    assert!(matches!(cross_resource, PersistenceError::RequestConflict));
    let session_request = MutationRequestId::from_bytes([0xa4; 16]);
    store
        .create_session(session_request, None)
        .await
        .expect("session should be created for cross-resource test");
    let reverse_cross_resource = store
        .set_open_code_credential(session_request, 1, RETRY_KEY.to_vec())
        .await
        .expect_err("session request identity cannot be reused for credential update");
    assert!(matches!(
        reverse_cross_resource,
        PersistenceError::RequestConflict
    ));
    drop(store);

    let database_bytes = fs::read(root.path().join("data").join("sessions.sqlite3"))
        .expect("database should be readable for secret scan");
    assert!(
        !database_bytes
            .windows(FIRST_KEY.len())
            .any(|window| window == FIRST_KEY)
    );
    assert!(
        !database_bytes
            .windows(RETRY_KEY.len())
            .any(|window| window == RETRY_KEY)
    );

    let reopened = SessionStore::open_at(root.path()).expect("credential state should reopen");
    assert_eq!(
        reopened
            .open_code_credential_status()
            .await
            .expect("reopened credential status should be readable"),
        configured
    );
    let remove_request = MutationRequestId::from_bytes([0xa3; 16]);
    let removed = reopened
        .remove_open_code_credential(remove_request, configured.generation)
        .await
        .expect("credential should be removed");
    assert_eq!(
        removed,
        OpenCodeCredentialStatus {
            configured: false,
            generation: 2,
        }
    );
    assert_eq!(
        reopened
            .remove_open_code_credential(remove_request, configured.generation)
            .await
            .expect("removal retry should return its prior result"),
        removed
    );
    drop(reopened);

    let reopened = SessionStore::open_at(root.path()).expect("removed state should reopen");
    assert_eq!(
        reopened
            .open_code_credential_status()
            .await
            .expect("removed status should be readable"),
        removed
    );
}

#[tokio::test(flavor = "current_thread")]
async fn startup_marks_prepared_credential_mutation_not_applied() {
    let root = TestRoot::new("credential-prepared-recovery");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let request_id = [0xb1; 16];
    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for crash setup");
    insert_credential_request_fixture(&connection, request_id, false);
    drop(connection);

    let recovered = SessionStore::open_at(root.path()).expect("prepared mutation should recover");
    assert_eq!(
        recovered
            .open_code_credential_status()
            .await
            .expect("credential status should be readable"),
        OpenCodeCredentialStatus {
            configured: false,
            generation: 0,
        }
    );
    let error = recovered
        .set_open_code_credential(
            MutationRequestId::from_bytes(request_id),
            0,
            b"not-a-real-recovery-key".to_vec(),
        )
        .await
        .expect_err("prepared mutation identity must not be reused");
    assert!(matches!(
        error,
        PersistenceError::CredentialMutationNotApplied
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn startup_completes_dispatched_installed_credential_mutation() {
    let root = TestRoot::new("credential-dispatched-recovery");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let request_id = [0xb2; 16];
    let database_path = root.path().join("data").join("sessions.sqlite3");
    let connection =
        Connection::open(&database_path).expect("database should open for crash setup");
    insert_credential_request_fixture(&connection, request_id, true);
    drop(connection);

    let mut credentials =
        CredentialStore::open(root.path()).expect("credential store should open for crash setup");
    credentials
        .apply(
            0,
            request_id,
            Some(
                StoredOpenCodeApiKey::new(b"not-a-real-installed-key".to_vec())
                    .expect("test key should be valid"),
            ),
        )
        .expect("credential file effect should be installed");
    drop(credentials);

    let recovered =
        SessionStore::open_at(root.path()).expect("dispatched mutation should complete on startup");
    let expected = OpenCodeCredentialStatus {
        configured: true,
        generation: 1,
    };
    assert_eq!(
        recovered
            .open_code_credential_status()
            .await
            .expect("recovered credential status should be readable"),
        expected
    );
    assert_eq!(
        recovered
            .set_open_code_credential(
                MutationRequestId::from_bytes(request_id),
                0,
                b"not-a-real-different-key".to_vec(),
            )
            .await
            .expect("completed mutation retry should return prior status"),
        expected
    );
}

#[tokio::test(flavor = "current_thread")]
async fn credential_commands_cross_the_application_and_transport_boundaries() {
    let root = TestRoot::new("credential-application-boundary");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let application = ServerApplication::from_session_store(store);
    let (client_connection, mut server_connection) = tokio::io::duplex(16 * 1024);

    let client_exchange = async {
        let mut client = ApplicationClient::from_negotiated_connection(client_connection);
        assert_eq!(
            client
                .open_code_credential_status()
                .await
                .expect("client should read credential status"),
            ProtocolOpenCodeCredentialStatus {
                configured: false,
                generation: 0,
            }
        );
        let configured = client
            .set_open_code_credential(
                ProtocolMutationRequestId::from_bytes([0xc1; 16]),
                0,
                OpenCodeApiKey::new("not-a-real-transport-key").expect("test key should be valid"),
            )
            .await
            .expect("client should configure a credential");
        assert_eq!(
            configured,
            ProtocolOpenCodeCredentialStatus {
                configured: true,
                generation: 1,
            }
        );
        let stale = client
            .remove_open_code_credential(ProtocolMutationRequestId::from_bytes([0xc2; 16]), 0)
            .await
            .expect_err("client should receive generation conflict");
        assert!(matches!(
            stale,
            ApplicationClientError::Application(ApplicationError::CredentialGenerationConflict)
        ));
        assert_eq!(
            client
                .remove_open_code_credential(
                    ProtocolMutationRequestId::from_bytes([0xc3; 16]),
                    configured.generation,
                )
                .await
                .expect("client should remove the credential"),
            ProtocolOpenCodeCredentialStatus {
                configured: false,
                generation: 2,
            }
        );
    };
    let server_exchange = async {
        handle_local_owner_requests(&mut server_connection, &application)
            .await
            .expect("server should handle credential requests");
    };

    tokio::join!(client_exchange, server_exchange);
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_or_unexpected_credential_state_fails_closed() {
    let malformed_root = TestRoot::new("malformed-credential");
    let malformed_store =
        SessionStore::open_at(malformed_root.path()).expect("session store should open");
    malformed_store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0xe2; 16]),
            0,
            b"not-a-real-malformed-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    drop(malformed_store);
    fs::write(
        malformed_root
            .path()
            .join("credentials")
            .join("opencode.state"),
        b"malformed",
    )
    .expect("credential fixture should be corrupted");
    let malformed = session_store_open_error(
        &malformed_root,
        "malformed credential state should fail closed",
    );
    assert!(matches!(malformed, PersistenceError::InvalidState { .. }));

    let unexpected_root = TestRoot::new("unexpected-credential-state");
    let unexpected_store =
        SessionStore::open_at(unexpected_root.path()).expect("session store should open");
    drop(unexpected_store);
    let unexpected_path = unexpected_root
        .path()
        .join("credentials")
        .join("unexpected");
    let mut unexpected_file =
        create_private_file(&unexpected_path).expect("unexpected fixture should be private");
    use std::io::Write as _;
    unexpected_file
        .write_all(b"unexpected")
        .expect("unexpected fixture should be written");
    unexpected_file
        .sync_all()
        .expect("unexpected fixture should be synchronized");
    drop(unexpected_file);
    let unexpected = session_store_open_error(
        &unexpected_root,
        "unexpected credential state should fail closed",
    );
    assert!(matches!(unexpected, PersistenceError::InvalidState { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn credential_history_corruption_fails_closed() {
    let root = TestRoot::new("credential-history-corruption");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    let request_id = MutationRequestId::from_bytes([0xe4; 16]);
    store
        .set_open_code_credential(request_id, 0, b"not-a-real-corruption-key".to_vec())
        .await
        .expect("credential should be configured");
    drop(store);

    let connection = Connection::open(root.path().join("data").join("sessions.sqlite3"))
        .expect("database should open for corruption");
    connection
        .execute(
            "DELETE FROM credential_audit_facts
             WHERE request_id = ?1 AND audit_kind = 1",
            [&request_id.as_bytes()[..]],
        )
        .expect("credential audit fixture should be corrupted");
    drop(connection);

    let error = session_store_open_error(&root, "credential corruption should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[test]
fn stale_private_credential_temporary_file_is_removed() {
    let root = TestRoot::new("stale-credential-temporary");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    drop(store);

    let temporary_path = root
        .path()
        .join("credentials")
        .join(".opencode.state-11111111111111111111111111111111.tmp");
    let mut temporary_file =
        create_private_file(&temporary_path).expect("temporary fixture should be private");
    use std::io::Write as _;
    temporary_file
        .write_all(b"incomplete")
        .expect("temporary fixture should be written");
    temporary_file
        .sync_all()
        .expect("temporary fixture should be synchronized");
    drop(temporary_file);

    let reopened = SessionStore::open_at(root.path()).expect("stale temporary file should clean");
    assert!(!temporary_path.exists());
    drop(reopened);
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn insecure_existing_credential_state_fails_closed() {
    let root = TestRoot::new("insecure-credential-state");
    let store = SessionStore::open_at(root.path()).expect("session store should open");
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0xe3; 16]),
            0,
            b"not-a-real-insecure-key".to_vec(),
        )
        .await
        .expect("credential should be configured");
    drop(store);

    let credential_path = root.path().join("credentials").join("opencode.state");
    fs::set_permissions(&credential_path, fs::Permissions::from_mode(0o644))
        .expect("test should weaken credential permissions");
    let error = session_store_open_error(&root, "insecure credential should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

fn insert_credential_request_fixture(
    connection: &Connection,
    request_id: [u8; 16],
    dispatched: bool,
) {
    connection
        .execute(
            "INSERT INTO mutation_requests (
                request_id,
                operation_kind,
                accepted_sequence,
                accepted_at_milliseconds
             ) VALUES (?1, 2, 1, 1000)",
            [&request_id[..]],
        )
        .expect("mutation registry fixture should be inserted");
    connection
        .execute(
            "INSERT INTO credential_mutation_requests (
                request_id,
                operation_kind,
                expected_generation,
                accepted_sequence,
                accepted_at_milliseconds,
                state,
                result_generation,
                result_configured
             ) VALUES (?1, 2, 0, 1, 1000, ?2, NULL, NULL)",
            params![&request_id[..], if dispatched { 1_i64 } else { 0_i64 }],
        )
        .expect("credential request fixture should be inserted");
    connection
        .execute(
            "INSERT INTO credential_audit_facts (
                audit_id,
                audit_sequence,
                request_id,
                actor_kind,
                audit_kind,
                created_at_milliseconds
             ) VALUES (?1, 2, ?2, 1, 1, 1000)",
            params![&[0xd1_u8; 16][..], &request_id[..]],
        )
        .expect("credential acceptance audit fixture should be inserted");
    let next_sequence = if dispatched {
        connection
            .execute(
                "INSERT INTO credential_operation_facts (
                    fact_id,
                    fact_sequence,
                    request_id,
                    operation_kind,
                    credential_generation,
                    created_at_milliseconds
                 ) VALUES (?1, 3, ?2, 1, 1, 1001)",
                params![&[0xd2_u8; 16][..], &request_id[..]],
            )
            .expect("credential dispatch fixture should be inserted");
        connection
            .execute(
                "INSERT INTO credential_audit_facts (
                    audit_id,
                    audit_sequence,
                    request_id,
                    actor_kind,
                    audit_kind,
                    created_at_milliseconds
                 ) VALUES (?1, 4, ?2, 1, 2, 1001)",
                params![&[0xd3_u8; 16][..], &request_id[..]],
            )
            .expect("credential dispatch audit fixture should be inserted");
        5_i64
    } else {
        3_i64
    };
    connection
        .execute(
            "UPDATE logical_sequences SET next_value = ?1 WHERE singleton = 1",
            [next_sequence],
        )
        .expect("logical sequence should advance past credential fixtures");
}

fn session_store_open_error(root: &TestRoot, message: &str) -> PersistenceError {
    match SessionStore::open_at(root.path()) {
        Ok(store) => {
            drop(store);
            panic!("{message}");
        }
        Err(error) => error,
    }
}

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(label: &str) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock should be after Unix epoch")
            .as_nanos();
        let sequence = TEST_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "morons-credential-{label}-{}-{timestamp}-{sequence}",
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

    fn path(&self) -> &Path {
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
