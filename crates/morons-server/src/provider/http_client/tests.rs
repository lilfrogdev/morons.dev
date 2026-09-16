use super::*;
use crate::provider::provider_cancellation;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn request(address: std::net::SocketAddr) -> Request<Full<Bytes>> {
    Request::post(format!("http://{address}/inference"))
        .header("x-client-request-id", "same-request")
        .body(Full::new(Bytes::from_static(b"fixed-body")))
        .unwrap()
}

fn unused_address() -> std::net::SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[tokio::test]
async fn connect_retry_succeeds_with_same_request() {
    let address = unused_address();
    let server = tokio::spawn(async move {
        time::sleep(Duration::from_millis(100)).await;
        let listener = TcpListener::bind(address).await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Vec::new();
        while !received.ends_with(b"fixed-body") {
            let mut buffer = [0; 1024];
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            received.extend_from_slice(&buffer[..count]);
            assert!(received.len() < 4096);
        }
        assert!(
            String::from_utf8(received)
                .unwrap()
                .contains("x-client-request-id: same-request")
        );
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
    });
    let (_, mut cancellation) = provider_cancellation();
    let response = send_model_request(
        &bounded_client(true, None),
        request(address),
        Duration::from_secs(2),
        Instant::now() + Duration::from_secs(5),
        &mut cancellation,
    )
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    server.await.unwrap();
}

#[tokio::test]
async fn connect_retries_are_capped() {
    let (_, mut cancellation) = provider_cancellation();
    let start = Instant::now();
    let error = time::timeout(
        Duration::from_secs(4),
        send_model_request(
            &bounded_client(true, None),
            request(unused_address()),
            Duration::from_secs(1),
            start + Duration::from_secs(10),
            &mut cancellation,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, ProviderError::Transport);
    assert!(start.elapsed() >= Duration::from_millis(1750));
}

#[tokio::test]
async fn connect_backoff_is_cancellable_and_deadline_bounded() {
    let client = bounded_client(true, None);
    let address = unused_address();
    let (handle, mut cancellation) = provider_cancellation();
    let cancel = tokio::spawn(async move {
        time::sleep(Duration::from_millis(50)).await;
        handle.cancel();
    });
    let error = time::timeout(
        Duration::from_millis(200),
        send_model_request(
            &client,
            request(address),
            Duration::from_secs(1),
            Instant::now() + Duration::from_secs(5),
            &mut cancellation,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, ProviderError::Cancelled);
    cancel.await.unwrap();
    let (_, mut cancellation) = provider_cancellation();
    let error = time::timeout(
        Duration::from_millis(200),
        send_model_request(
            &client,
            request(address),
            Duration::from_secs(1),
            Instant::now() + Duration::from_millis(50),
            &mut cancellation,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, ProviderError::TotalTimeout);
}

#[tokio::test]
async fn transmitted_requests_are_not_retried() {
    for mode in 0..3 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            assert!(socket.read(&mut buffer).await.unwrap() > 0);
            match mode {
                0 => socket
                    .write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .await
                    .unwrap(),
                1 => {}
                _ => time::sleep(Duration::from_millis(100)).await,
            }
            drop(socket);
            assert!(
                time::timeout(Duration::from_millis(350), listener.accept())
                    .await
                    .is_err()
            );
        });
        let (_, mut cancellation) = provider_cancellation();
        let result = send_model_request(
            &bounded_client(true, None),
            request(address),
            Duration::from_millis(50),
            Instant::now() + Duration::from_secs(2),
            &mut cancellation,
        )
        .await;
        match mode {
            0 => assert_eq!(result.unwrap().status(), 503),
            1 => assert_eq!(result.unwrap_err(), ProviderError::Transport),
            _ => assert_eq!(result.unwrap_err(), ProviderError::ResponseHeaderTimeout),
        }
        server.await.unwrap();
    }
}
