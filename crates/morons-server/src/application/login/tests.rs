use super::*;
use crate::{
    persistence::{MutationRequestId, SessionStore, credential_tests::TestRoot},
    provider::openai_auth::OAuthTokens,
};
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};
#[tokio::test(flavor = "current_thread")]
async fn authenticated_controls_do_not_disclose_tokens_cancel_other_connections_or_remove_opencode()
{
    let root = TestRoot::new("auth-controls");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([1; 16]),
            0,
            b"opencode-fixture".to_vec(),
        )
        .await
        .unwrap();
    store
        .set_openai_credential(
            MutationRequestId::from_bytes([2; 16]),
            0,
            OAuthTokens::fixture(
                "account-fixture",
                "refresh-fixture",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs()
                    + 3600,
            ),
        )
        .await
        .unwrap();
    let opencode = fs::read(root.path().join("credentials/opencode.state")).unwrap();
    let app = ServerApplication::from_session_store(store);
    let ApplicationOutcome::Response(response) = app
        .execute_for_local_owner(ApplicationRequest::GetOpenAiCredentialStatus)
        .await
        .unwrap()
    else {
        panic!("status expected")
    };
    assert_eq!(
        response,
        ApplicationResponse::OpenAiCredentialStatus {
            credential: morons_protocol::OpenAiCredentialStatus {
                generation: 1,
                state: morons_protocol::OpenAiCredentialState::Configured
            }
        }
    );
    let encoded = serde_json::to_string(&response).unwrap();
    assert!(!encoded.contains("account-fixture"));
    assert!(!encoded.contains("refresh-fixture"));
    assert!(matches!(
        app.execute_for_local_owner(ApplicationRequest::CancelOpenAiLogin {
            attempt_id: morons_protocol::MutationRequestId::from_bytes([2; 16])
        })
        .await,
        Err(ApplicationError::InvalidRequest)
    ));
    assert!(matches!(
        app.execute_for_local_owner(ApplicationRequest::RemoveOpenAiCredential {
            mutation_request_id: morons_protocol::MutationRequestId::from_bytes([3; 16]),
            expected_generation: 0
        })
        .await,
        Err(ApplicationError::CredentialGenerationConflict)
    ));
    let ApplicationOutcome::Response(response) = app
        .execute_for_local_owner(ApplicationRequest::RemoveOpenAiCredential {
            mutation_request_id: morons_protocol::MutationRequestId::from_bytes([3; 16]),
            expected_generation: 1,
        })
        .await
        .unwrap()
    else {
        panic!("removal expected")
    };
    assert_eq!(
        response,
        ApplicationResponse::OpenAiCredentialStatus {
            credential: morons_protocol::OpenAiCredentialStatus {
                generation: 2,
                state: morons_protocol::OpenAiCredentialState::Unconfigured
            }
        }
    );
    assert_eq!(
        fs::read(root.path().join("credentials/opencode.state")).unwrap(),
        opencode
    );
    app.shutdown().await;
    assert!(matches!(
        app.execute_for_local_owner(ApplicationRequest::BeginOpenAiLogin {
            mutation_request_id: morons_protocol::MutationRequestId::from_bytes([4; 16]),
            expected_generation: 2
        })
        .await,
        Err(ApplicationError::ServiceUnavailable)
    ));
}
