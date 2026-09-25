use super::*;

#[tokio::test(flavor = "current_thread")]
async fn context_rejection_requires_complete_bounded_body_and_never_replays() {
    let (_root, _store, provider, mut listener) = setup().await;
    for case in [
        "complete",
        "truncated",
        "oversized",
        "chunked_complete",
        "chunked_truncated",
        "chunked_oversized",
        "chunked_trailers",
    ] {
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = mock_request(&mut socket).await;
            let body = br#"{"error":{"code":"context_length_exceeded","message":"private-error-fixture"}}"#;
            match case {
                "complete" => respond(&mut socket, "400 Bad Request", "", body).await,
                "truncated" => {
                    socket.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len() + 1).as_bytes()).await.unwrap();
                    socket.write_all(body).await.unwrap();
                }
                "oversized" => {
                    let mut oversized = body.to_vec();
                    oversized.resize(65_537, b' ');
                    respond(&mut socket, "400 Bad Request", "", &oversized).await;
                }
                "chunked_complete" | "chunked_truncated" | "chunked_oversized"
                | "chunked_trailers" => {
                    socket.write_all(b"HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                    let mut payload = body.to_vec();
                    if case == "chunked_oversized" {
                        payload.resize(65_537, b' ');
                    }
                    for chunk in payload.chunks(1024) {
                        socket
                            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                            .await
                            .unwrap();
                        socket.write_all(chunk).await.unwrap();
                        socket.write_all(b"\r\n").await.unwrap();
                    }
                    match case {
                        "chunked_truncated" => {}
                        "chunked_trailers" => socket
                            .write_all(b"0\r\nx-untrusted: value\r\n\r\n")
                            .await
                            .unwrap(),
                        _ => socket.write_all(b"0\r\n\r\n").await.unwrap(),
                    }
                }
                _ => unreachable!(),
            }
            drop(socket);
            listener
        });
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
        let request = request(&turn);
        let (_, mut cancel) = provider_cancellation();
        let error = provider
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel,
            )
            .await
            .unwrap()
            .execute(DataUseRestrictions::default(), &mut cancel, |_| {
                panic!("rejection emitted output")
            })
            .await
            .unwrap_err();
        assert_eq!(
            error,
            match case {
                "complete" | "chunked_complete" => ProviderError::ContextWindowRejected,
                "truncated" | "chunked_truncated" => ProviderError::Transport,
                "oversized" | "chunked_oversized" => ProviderError::ResponseLimitExceeded,
                "chunked_trailers" => ProviderError::MalformedResponse,
                _ => unreachable!(),
            }
        );
        assert!(!format!("{error:?} {turn:?}").contains("private-error-fixture"));
        assert!(
            provider
                .prepare_dispatch(
                    &mut turn,
                    &request,
                    DataUseRestrictions::default(),
                    &mut cancel
                )
                .await
                .is_err()
        );
        listener = peer.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
