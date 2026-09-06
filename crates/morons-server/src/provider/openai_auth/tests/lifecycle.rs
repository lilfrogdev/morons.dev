use super::*;

#[tokio::test(flavor = "current_thread")]
async fn production_admission_is_nonqueued_before_binding_or_contacting_any_service() {
    let permit = LOGIN_SLOT.clone().try_acquire_owned().unwrap();
    assert_eq!(OpenAiLogin::begin().await.unwrap_err(), OAuthError::Busy);
    drop(permit);
    assert_eq!(LOGIN_SLOT.available_permits(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_state_does_not_consume_the_callback_or_authorize_an_exchange() {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slot = Arc::new(Semaphore::new(1));
    let uri = format!("http://{}/oauth/token", provider.local_addr().unwrap())
        .parse()
        .unwrap();
    let login = login(uri, &slot, Duration::from_secs(3)).await;
    let address = login.callback.address();
    let state = login.state.to_string();
    let (_, mut cancel) = provider_cancellation();
    let task = tokio::spawn(async move { login.complete(&mut cancel).await });
    let rejected = submit(address, "wrong-state".to_owned(), "wrong-code").await;
    assert!(rejected.starts_with(b"HTTP/1.1 400"));
    assert!(!String::from_utf8(rejected).unwrap().contains("wrong-code"));
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
    let good = submit(address, state, "code-fixture");
    let exchange = async {
        let (mut stream, _) = provider.accept().await.unwrap();
        let _ = mock_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    };
    let (_, ()) = tokio::join!(good, exchange);
    assert_eq!(task.await.unwrap().unwrap_err(), OAuthError::TokenRejected);
    assert_eq!(slot.available_permits(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn token_headers_have_an_independent_timeout_without_retry() {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = TokenClient::for_test(
        format!("http://{}/oauth/token", provider.local_addr().unwrap())
            .parse()
            .unwrap(),
    );
    let exchange = async {
        let (mut stream, _) = provider.accept().await.unwrap();
        let _ = mock_request(&mut stream).await;
        let mut bytes = [0];
        assert_eq!(stream.read(&mut bytes).await.unwrap(), 0);
    };
    let future = client.exchange(Zeroizing::new("grant_type=fixture".to_owned()));
    let (result, ()) = time::timeout(Duration::from_secs(15), async {
        tokio::join!(future, exchange)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), OAuthError::ExchangeUncertain);
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
}
