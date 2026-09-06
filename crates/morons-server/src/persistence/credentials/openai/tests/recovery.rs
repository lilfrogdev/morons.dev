use super::*;
use crate::persistence::credential_tests::insert_credential_request_fixture;
use rusqlite::Connection;

#[tokio::test(flavor = "current_thread")]
async fn owner_mutation_recovery_never_reapplies_tokens_and_preserves_provider_scope() {
    for (dispatched, installed) in [(false, false), (true, false), (true, true)] {
        let root = TestRoot::new("oauth-owner-recovery");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        drop(store);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        insert_credential_request_fixture(&db, [9; 16], dispatched);
        db.execute(
            "UPDATE credential_mutation_requests SET credential_kind=2",
            [],
        )
        .unwrap();
        drop(db);
        if installed {
            let mut file = OpenAiCredentialStore::open(root.path()).unwrap();
            file.apply(0, [9; 16], Some(tokens()), now()).unwrap();
        }
        let store = SessionStore::open_for_test(root.path()).unwrap();
        let status = store.openai_credential_status().await.unwrap();
        assert_eq!(status.generation, u64::from(installed));
        assert_eq!(
            store
                .open_code_credential_status()
                .await
                .unwrap()
                .generation,
            0
        );
        let retry = store
            .set_openai_credential(
                MutationRequestId::from_bytes([9; 16]),
                0,
                OAuthTokens::fixture("unrelated", "new-secret", now() + 3600),
            )
            .await;
        if installed {
            assert_eq!(retry.unwrap().generation, 1);
        } else {
            assert!(matches!(
                retry,
                Err(PersistenceError::CredentialMutationNotApplied)
            ));
        }
        drop(store);
        if installed {
            let bytes = fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap();
            assert!(!bytes.windows(10).any(|b| b == b"new-secret"));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dispatched_refresh_is_disabled_on_restart_without_network_or_generation_change() {
    let (root, store) = configured("oauth-restart-refresh").await;
    let lease = store.begin_openai_access(1).await.unwrap();
    drop(lease);
    drop(store);
    let reopened = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        reopened.openai_credential_status().await.unwrap(),
        OpenAiCredentialStatus {
            generation: 1,
            state: OpenAiCredentialState::ReauthenticationRequired
        }
    );
    let file =
        format::decode(&fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap())
            .unwrap();
    assert_eq!(file.revision, 1);
    assert_eq!(file.mutation, [1; 16]);
    reopened
        .remove_openai_credential(MutationRequestId::from_bytes([3; 16]), 1)
        .await
        .unwrap();
    assert_eq!(
        reopened
            .openai_credential_status()
            .await
            .unwrap()
            .generation,
        2
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_missing_or_rebound_credentials_fail_closed() {
    for damage in ["bytes", "missing", "generation", "audit"] {
        let (root, store) = configured(damage).await;
        drop(store);
        let path = root.path().join("credentials/openai-chatgpt.state");
        match damage {
            "bytes" => {
                let mut bytes = fs::read(&path).unwrap();
                bytes[60] ^= 1;
                fs::write(&path, bytes).unwrap();
            }
            "missing" => fs::remove_file(&path).unwrap(),
            "generation" => {
                let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
                db.execute("UPDATE credential_mutation_requests SET expected_generation=5 WHERE credential_kind=2",[]).unwrap();
            }
            _ => {
                let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
                db.execute("DELETE FROM credential_audit_facts WHERE audit_kind=3", [])
                    .unwrap();
            }
        }
        assert!(SessionStore::open_for_test(root.path()).is_err());
    }
}

#[test]
fn only_exact_private_temporary_names_are_cleaned() {
    let root = TestRoot::new("oauth-temp-names");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    drop(store);
    let directory = root.path().join("credentials");
    let temp = directory.join(files::temporary(CredentialKind::OpenAiChatGpt, &[8; 16]));
    drop(crate::persistence::paths::create_private_file(&temp).unwrap());
    drop(SessionStore::open_for_test(root.path()).unwrap());
    assert!(!temp.exists());
    let unknown = directory.join(".openai-chatgpt.state-not-a-marker.tmp");
    drop(crate::persistence::paths::create_private_file(&unknown).unwrap());
    assert!(SessionStore::open_for_test(root.path()).is_err());
    assert!(unknown.exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn insecure_and_linked_oauth_files_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (root, store) = configured("oauth-private-file").await;
    drop(store);
    let path = root.path().join("credentials/openai-chatgpt.state");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(SessionStore::open_for_test(root.path()).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let outside = root.path().join("retained-token-fixture");
    fs::rename(&path, &outside).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(SessionStore::open_for_test(root.path()).is_err());
    assert!(outside.exists());
}

#[tokio::test(flavor = "current_thread")]
async fn schema_27_migration_preserves_opencode_bytes_and_does_not_invent_oauth_identity() {
    let root = TestRoot::new("oauth-migration");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([8; 16]),
            0,
            b"legacy-opencode-fixture".to_vec(),
        )
        .await
        .unwrap();
    drop(store);
    let key = fs::read(root.path().join("credentials/opencode.state")).unwrap();
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    db.pragma_update(None, "foreign_keys", false).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; PRAGMA defer_foreign_keys=ON;
        CREATE TABLE credential_mutation_requests_v27 (
            request_id BLOB PRIMARY KEY NOT NULL REFERENCES mutation_requests(request_id),
            operation_kind INTEGER NOT NULL CHECK(operation_kind IN(2,3)),
            expected_generation INTEGER NOT NULL CHECK(expected_generation>=0),
            accepted_sequence INTEGER NOT NULL UNIQUE CHECK(accepted_sequence>0),
            accepted_at_milliseconds INTEGER NOT NULL CHECK(accepted_at_milliseconds>=0),
            state INTEGER NOT NULL CHECK(state IN(0,1,2,3)),
            result_generation INTEGER UNIQUE CHECK(result_generation IS NULL OR result_generation>0),
            result_configured INTEGER CHECK(result_configured IS NULL OR result_configured IN(0,1)),
            CHECK((state=2 AND result_generation IS NOT NULL AND result_configured IS NOT NULL) OR (state!=2 AND result_generation IS NULL AND result_configured IS NULL))
        ) STRICT, WITHOUT ROWID;
        INSERT INTO credential_mutation_requests_v27 SELECT request_id,operation_kind,expected_generation,accepted_sequence,accepted_at_milliseconds,state,result_generation,result_configured FROM credential_mutation_requests;
        DROP TABLE credential_mutation_requests;
        ALTER TABLE credential_mutation_requests_v27 RENAME TO credential_mutation_requests;
        CREATE INDEX credential_mutation_requests_by_state ON credential_mutation_requests(state,accepted_sequence);
        PRAGMA user_version=27;COMMIT;").unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    drop(db);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store
            .open_code_credential_status()
            .await
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        0
    );
    assert_eq!(
        fs::read(root.path().join("credentials/opencode.state")).unwrap(),
        key
    );
    assert!(
        !root
            .path()
            .join("credentials/openai-chatgpt.state")
            .exists()
    );
    drop(store);
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v27.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        27
    );
}
