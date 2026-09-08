use super::*;
use crate::provider::response_diagnostic::ResponseStage;

#[tokio::test(flavor = "current_thread")]
async fn native_diagnostic_captures_only_a_stage_and_keeps_error_and_no_replay_semantics() {
    for (case, expected, error) in [
        (
            "duplicate",
            ResponseStage::HeaderFraming,
            ProviderError::MalformedResponse,
        ),
        (
            "content-type",
            ResponseStage::ContentType,
            ProviderError::UnexpectedContentType,
        ),
        (
            "routing",
            ResponseStage::RoutingState,
            ProviderError::MalformedResponse,
        ),
        (
            "sequence",
            ResponseStage::Sequence,
            ProviderError::MalformedResponse,
        ),
        (
            "model",
            ResponseStage::ResponseModel,
            ProviderError::MalformedResponse,
        ),
        (
            "usage",
            ResponseStage::Usage,
            ProviderError::MalformedResponse,
        ),
        (
            "termination",
            ResponseStage::Termination,
            ProviderError::IncompleteResponse,
        ),
        (
            "redirect",
            ResponseStage::Redirect,
            ProviderError::RedirectDenied,
        ),
    ] {
        let (_root, _store, provider, listener) = setup().await;
        let peer = tokio::spawn(async move {
            let (mut socket, _) = time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let _ = mock_request(&mut socket).await;
            let mut body = String::from_utf8(sse(if case == "usage" { 33 } else { 5 })).unwrap();
            match case {
                "sequence" => body = body.replace("\"sequence_number\":0", "\"sequence_number\":9"),
                "model" => body = body.replace("gpt-5.5", "PRIVATE-model"),
                "termination" => body = body.split("\n\n").next().unwrap().to_owned() + "\n\n",
                _ => {}
            }
            let headers = match case {
                "duplicate" => "Content-Type: text/event-stream\r\n",
                "routing" => {
                    "x-codex-turn-state: PRIVATE-first\r\nx-codex-turn-state: PRIVATE-second\r\n"
                }
                "redirect" => "Location: https://example.invalid/PRIVATE\r\n",
                _ => "",
            };
            if case == "content-type" {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/PRIVATE\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            } else {
                respond(
                    &mut socket,
                    if case == "redirect" {
                        "302 Found"
                    } else {
                        "200 OK"
                    },
                    headers,
                    body.as_bytes(),
                )
                .await;
            }
            listener
        });
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
        assert_eq!(turn.response_failure(), None);
        let request = request(&turn);
        let (_, mut cancel) = provider_cancellation();
        let result = provider
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel,
            )
            .await
            .unwrap()
            .execute(DataUseRestrictions::default(), &mut cancel, |_| {})
            .await;
        assert_eq!(result.err(), Some(error));
        assert_eq!(turn.response_failure(), Some(expected));
        assert!(!format!("{turn:?} {error:?} {:?}", turn.response_failure()).contains("PRIVATE"));
        assert!(!turn.usable);
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
        let listener = peer.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
