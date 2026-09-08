use super::*;

#[tokio::test(flavor = "current_thread")]
async fn token_transport_rejection_stages_are_fixed_and_never_retried() {
    use TokenResponseFailure as R;
    for (response, reason) in [
        (
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\n\r\n",
            R::Headers,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 65537\r\n\r\n",
            R::BodyBounds,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n0\r\nX-Private: PRIVATE\r\n\r\n",
            R::BodyFraming,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 7\r\n\r\nPRIVATE",
            R::Json,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            R::TokenFields,
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TokenClient::for_test(
            format!("http://{}/oauth/token", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        let reply = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = mock_request(&mut stream).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        };
        let (result, ()) = tokio::join!(
            client.exchange(Zeroizing::new("grant_type=fixture".into())),
            reply
        );
        let error = result.unwrap_err();
        assert_eq!(error, OAuthError::InvalidTokenResponse(reason));
        assert!(!error.to_string().contains("PRIVATE"));
        assert!(!format!("{error:?}").contains("PRIVATE"));
        assert!(
            time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
