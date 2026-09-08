use crate::{
    application::ServerApplication,
    handle_local_owner_requests,
    persistence::{SessionStore, credential_tests::TestRoot},
};
use morons_cli::{ApplicationClient, ApplicationClientError};
use morons_protocol::{ApplicationError, DataUsePolicy, MutationRequestId};

#[tokio::test]
async fn post_authentication_policy_ipc_is_sequence_scoped_and_durable() {
    let root = TestRoot::new("policy-ipc");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let application = ServerApplication::from_session_store(store);
    let (stream, mut server) = tokio::io::duplex(16 * 1024);
    let server = tokio::spawn(async move {
        handle_local_owner_requests(&mut server, &application)
            .await
            .unwrap();
        application.shutdown().await;
    });
    let mut client = ApplicationClient::from_negotiated_connection(stream);
    assert_eq!(
        client.application_settings().await.unwrap().data_use,
        DataUsePolicy::default()
    );
    let id = MutationRequestId::from_bytes([0xc1; 16]);
    let intent = DataUsePolicy {
        sequence: 0,
        block_training_use: true,
        require_zero_retention: false,
    };
    let saved = client.set_data_use_policy(id, intent).await.unwrap();
    assert!(saved.data_use.sequence > 0);
    assert!(saved.data_use.block_training_use);
    assert!(!saved.data_use.require_zero_retention);
    assert_eq!(client.set_data_use_policy(id, intent).await.unwrap(), saved);
    assert!(matches!(
        client
            .set_data_use_policy(
                MutationRequestId::from_bytes([0xc2; 16]),
                DataUsePolicy {
                    require_zero_retention: true,
                    ..intent
                }
            )
            .await,
        Err(ApplicationClientError::Application(
            ApplicationError::DataUsePolicyChanged
        ))
    ));
    assert_eq!(client.application_settings().await.unwrap(), saved);
    drop(client);
    server.await.unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store.data_use_policy().await.unwrap().sequence,
        saved.data_use.sequence
    );
}
