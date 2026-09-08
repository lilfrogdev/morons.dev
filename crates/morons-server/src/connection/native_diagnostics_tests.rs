use super::*;
use crate::{
    application::events::NativeResponseDiagnostic,
    persistence::{MutationRequestId, SessionStore, credential_tests::TestRoot},
    provider::response_diagnostic::ResponseStage,
};

#[tokio::test]
async fn lagging_native_diagnostic_subscriber_is_disconnected() {
    let root = TestRoot::new("native-diagnostic-lag");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([1; 16]),
            None,
            root.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let app = ServerApplication::from_session_store(store);
    let mut cursor = [0; 24];
    cursor[..16].copy_from_slice(session.id.as_bytes());
    let ApplicationOutcome::SessionSubscription(mut subscription) = app
        .execute_for_local_owner(morons_protocol::ApplicationRequest::SubscribeSession {
            session_id: morons_protocol::SessionId::from_bytes(*session.id.as_bytes()),
            cursor: morons_protocol::SessionEventCursor::from_bytes(cursor),
        })
        .await
        .unwrap()
    else {
        panic!("subscription expected")
    };
    let (sender, receiver) = tokio::sync::broadcast::channel(1);
    subscription.native_diagnostics = receiver;
    for _ in 0..2 {
        sender
            .send(NativeResponseDiagnostic {
                session_id: session.id,
                run_id: crate::persistence::RunId::from_bytes([2; 16]),
                reason: ResponseStage::Usage,
            })
            .unwrap();
    }
    let (_client, mut server) = tokio::io::duplex(1024);
    let result = time::timeout(
        Duration::from_secs(10),
        stream_session_events(&mut server, &app, subscription),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(ConnectionError::SubscriberLagged)));
    app.shutdown().await;
}
