use super::super::*;
use crate::provider::{
    openai_auth::{
        form,
        tests::{login, submit_query},
    },
    provider_cancellation,
};
use std::sync::Arc;
use tokio::sync::Semaphore;

#[tokio::test(flavor = "current_thread")]
async fn valid_provider_denial_has_a_static_response_and_never_exchanges() {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slot = Arc::new(Semaphore::new(1));
    let login = login(
        format!("http://{}/oauth/token", provider.local_addr().unwrap())
            .parse()
            .unwrap(),
        &slot,
        Duration::from_secs(3),
    )
    .await;
    let address = login.callback.address();
    let query = form(&[
        ("error", "access_denied"),
        ("error_description", "PRIVATE-DESCRIPTION"),
        ("state", login.state.as_str()),
        ("iss", "https://auth.openai.com"),
        ("error_uri", "https://ignored.invalid/"),
    ]);
    let (_, mut cancellation) = provider_cancellation();
    let (reply, result) = tokio::join!(
        submit_query(address, &query),
        login.complete(&mut cancellation)
    );
    assert_eq!(reply, DENIED);
    assert_eq!(result.unwrap_err(), OAuthError::AuthorizationDenied);
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
    assert!(TcpStream::connect(address).await.is_err());
    assert_eq!(slot.available_permits(), 1);
}
