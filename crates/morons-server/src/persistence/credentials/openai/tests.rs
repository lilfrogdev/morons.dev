mod failures;
mod recovery;
mod refresh;
use super::*;
use crate::persistence::{MutationRequestId, SessionStore, credential_tests::TestRoot};
use std::{
    fs,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn tokens() -> OAuthTokens {
    OAuthTokens::fixture("account-fixture", "refresh-fixture", now() + 3600)
}
async fn configured(label: &str) -> (TestRoot, Arc<SessionStore>) {
    let root = TestRoot::new(label);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_openai_credential(MutationRequestId::from_bytes([1; 16]), 0, tokens())
        .await
        .unwrap();
    drop(store);
    // Advance only the synthetic credential's effective expiry, never real credential state.
    let directory = root.path().join("credentials");
    let path = files::path(&directory, CredentialKind::OpenAiChatGpt);
    let mut state = format::decode(&files::read(&path, format::MAX_FILE).unwrap()).unwrap();
    state.tokens = Some(OAuthTokens::fixture(
        "account-fixture",
        "refresh-fixture",
        now() - 1,
    ));
    files::replace(
        &directory,
        CredentialKind::OpenAiChatGpt,
        &[4; 16],
        &format::encode(&state).unwrap(),
    )
    .unwrap();
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    (root, store)
}

#[tokio::test(flavor = "current_thread")]
async fn identities_are_independent_idempotent_and_secret_free_in_sqlite() {
    let root = TestRoot::new("oauth-identities");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store.openai_credential_status().await.unwrap().state,
        OpenAiCredentialState::Unconfigured
    );
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([1; 16]),
            0,
            b"opencode-fixture".to_vec(),
        )
        .await
        .unwrap();
    let id = MutationRequestId::from_bytes([2; 16]);
    let status = store.set_openai_credential(id, 0, tokens()).await.unwrap();
    assert_eq!(status.generation, 1);
    assert!(status.configured);
    assert_eq!(
        store
            .open_code_credential_status()
            .await
            .unwrap()
            .generation,
        1
    );
    assert!(matches!(
        store
            .set_openai_credential(MutationRequestId::from_bytes([1; 16]), 0, tokens())
            .await,
        Err(PersistenceError::RequestConflict)
    ));
    let before = fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap();
    let retry = OAuthTokens::fixture("other-account", "different-refresh", now() + 3600);
    assert_eq!(
        store.set_openai_credential(id, 0, retry).await.unwrap(),
        status
    );
    assert_eq!(
        fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap(),
        before
    );
    assert!(matches!(
        store
            .remove_openai_credential(MutationRequestId::from_bytes([3; 16]), 0)
            .await,
        Err(PersistenceError::CredentialGenerationConflict)
    ));
    let api = fs::read(root.path().join("credentials/opencode.state")).unwrap();
    store
        .remove_openai_credential(MutationRequestId::from_bytes([3; 16]), 1)
        .await
        .unwrap();
    assert_eq!(
        fs::read(root.path().join("credentials/opencode.state")).unwrap(),
        api
    );
    assert_eq!(
        store.openai_credential_status().await.unwrap(),
        OpenAiCredentialStatus {
            generation: 2,
            state: OpenAiCredentialState::Unconfigured
        }
    );
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store
            .open_code_credential_status()
            .await
            .unwrap()
            .generation,
        1
    );
    drop(store);
    let db = fs::read(root.path().join("data/sessions.sqlite3")).unwrap();
    for secret in [
        b"account-fixture".as_slice(),
        b"refresh-fixture",
        b"opencode-fixture",
        b"other-account",
        b"different-refresh",
    ] {
        assert!(!db.windows(secret.len()).any(|b| b == secret));
    }
    let removed = fs::read(root.path().join("credentials/openai-chatgpt.state")).unwrap();
    assert!(
        !removed
            .windows(b"refresh-fixture".len())
            .any(|b| b == b"refresh-fixture")
    );
}

#[test]
fn credential_binary_is_bounded_checked_and_redacted() {
    let state = State {
        generation: 1,
        revision: 1,
        mutation: [1; 16],
        refresh: [0; 16],
        phase: Phase::Active,
        tokens: Some(OAuthTokens::fixture(
            "account-fixture",
            "refresh-fixture",
            1,
        )),
    };
    let bytes = format::encode(&state).unwrap();
    let restored = format::decode(&bytes).unwrap();
    assert_eq!(restored.generation, 1);
    assert_eq!(restored.tokens.unwrap().expires_at_seconds(), 1);
    assert!(!format!("{state:?}").contains("account-fixture"));
    for index in [0, 40, bytes.len() - 1] {
        let mut bad = bytes.to_vec();
        bad[index] ^= 1;
        assert!(format::decode(&bad).is_err());
    }
    assert!(format::decode(&bytes[..bytes.len() - 1]).is_err());
    assert!(format::decode(&vec![0; format::MAX_FILE + 1]).is_err());
}
