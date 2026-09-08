use super::*;

#[tokio::test(flavor = "current_thread")]
async fn native_response_metadata_and_stream_bounds_fail_closed() {
    for case in [
        "routing-change",
        "large-header",
        "large-body",
        "wrong-model",
        "content-type",
    ] {
        let (_root, _store, provider, listener) = setup().await;
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
        if case == "routing-change" {
            turn.routing = Some(http::HeaderValue::from_static("first"));
        }
        let request = request(&turn);
        let (_, mut cancel) = provider_cancellation();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = mock_request(&mut socket).await;
            let body = if case == "wrong-model" {
                String::from_utf8(sse(5))
                    .unwrap()
                    .replace("gpt-5.5", "gpt-5.4")
                    .into_bytes()
            } else {
                sse(5)
            };
            let extra = match case {
                "routing-change" => "x-codex-turn-state: second\r\n".to_owned(),
                "large-header" => format!("x-padding: {}\r\n", "x".repeat(20000)),
                "content-type" => "Content-Type: application/json\r\n".to_owned(),
                _ => String::new(),
            };
            let length = if case == "large-body" {
                16777217
            } else {
                body.len()
            };
            let mut response=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n{extra}\r\n").into_bytes();
            response.extend(body);
            let _ = socket.write_all(&response).await;
        });
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
        assert!(result.is_err(), "{case}");
        assert!(!turn.usable);
        peer.await.unwrap();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn policy_and_credential_scope_are_rechecked_before_any_inference_bytes() {
    let (_root, store, provider, listener) = setup().await;
    let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    let request = request(&turn);
    let (_, mut cancel) = provider_cancellation();
    let restricted = DataUseRestrictions {
        block_training_use: true,
        require_zero_retention: false,
    };
    assert!(matches!(
        provider
            .prepare_dispatch(&mut turn, &request, restricted, &mut cancel)
            .await,
        Err(ProviderError::DataUseRestricted)
    ));
    let prepared = provider
        .prepare_dispatch(
            &mut turn,
            &request,
            DataUseRestrictions::default(),
            &mut cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        prepared.execute(restricted, &mut cancel, |_| {}).await,
        Err(ProviderError::DataUseRestricted)
    ));
    assert!(turn.usable);
    assert_eq!(turn.sequence, 0);
    let unrelated =
        OpenAiCodexProvider::new(Arc::new(OpenAiCredentialProvider::new(store.clone())));
    assert!(matches!(
        unrelated
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel
            )
            .await,
        Err(ProviderError::InvalidRequest)
    ));
    store
        .set_openai_credential(MutationRequestId::from_bytes([2; 16]), 1, tokens())
        .await
        .unwrap();
    assert!(matches!(
        provider
            .prepare_dispatch(
                &mut turn,
                &request,
                DataUseRestrictions::default(),
                &mut cancel
            )
            .await,
        Err(ProviderError::CredentialGenerationChanged)
    ));
    assert!(
        time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn credential_lease_is_released_after_headers_without_rebinding_the_in_flight_turn() {
    let (_root, store, provider, listener) = setup().await;
    let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    let request = request(&turn);
    let (_, mut cancel) = provider_cancellation();
    let (sent, received) = oneshot::channel();
    let (release, ready) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = mock_request(&mut socket).await;
        let body = sse(5);
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        sent.send(()).unwrap();
        ready.await.unwrap();
        socket.write_all(&body).await.unwrap();
    });
    let prepared = provider
        .prepare_dispatch(
            &mut turn,
            &request,
            DataUseRestrictions::default(),
            &mut cancel,
        )
        .await
        .unwrap();
    let mutation = async {
        received.await.unwrap();
        time::timeout(
            Duration::from_secs(5),
            store.set_openai_credential(MutationRequestId::from_bytes([3; 16]), 1, tokens()),
        )
        .await
        .unwrap()
        .unwrap();
        release.send(()).unwrap();
    };
    let (outcome, ()) = tokio::join!(
        prepared.execute(DataUseRestrictions::default(), &mut cancel, |_| {}),
        mutation
    );
    outcome.unwrap();
    peer.await.unwrap();
    assert_eq!(turn.identity.generation, 1);
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        2
    );
}
#[tokio::test(flavor = "current_thread")]
async fn response_header_and_total_deadlines_close_without_retry() {
    for stage in ["headers", "body"] {
        let (_root, _store, mut provider, listener) = setup().await;
        Arc::get_mut(&mut provider).unwrap().set_timeouts_for_test(
            if stage == "headers" {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(1)
            },
            if stage == "body" {
                Duration::from_millis(200)
            } else {
                Duration::from_secs(1)
            },
        );
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
        let request = request(&turn);
        let (_, mut cancel) = provider_cancellation();
        let peer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let _ = mock_request(&mut socket).await;
            if stage == "body" {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 100\r\n\r\n").await.unwrap();
            }
            let mut byte = [0];
            assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
            listener
        });
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
        assert_eq!(
            result.unwrap_err(),
            if stage == "headers" {
                ProviderError::ResponseHeaderTimeout
            } else {
                ProviderError::TotalTimeout
            }
        );
        assert!(!turn.usable);
        let listener = peer.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
#[tokio::test(flavor = "current_thread")]
async fn reasoning_continuation_is_receipt_bound_and_cannot_cross_turns() {
    let (_root, _store, provider, listener) = setup().await;
    let mut turn = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    let request = request(&turn);
    let (_, mut cancel) = provider_cancellation();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = mock_request(&mut socket).await;
        respond(&mut socket, "200 OK", "", &sse(5)).await;
    });
    let outcome = provider
        .prepare_dispatch(
            &mut turn,
            &request,
            DataUseRestrictions::default(),
            &mut cancel,
        )
        .await
        .unwrap()
        .execute(DataUseRestrictions::default(), &mut cancel, |_| {})
        .await
        .unwrap();
    peer.await.unwrap();
    let reasoning = outcome
        .output
        .into_iter()
        .find_map(|item| {
            if let ProviderOutputItem::Reasoning(r) = item {
                Some(r)
            } else {
                None
            }
        })
        .unwrap();
    let continuation = ProviderInputItem::Reasoning {
        id: reasoning.provider_item_id,
        summaries: reasoning.summaries,
        encrypted_content: reasoning.encrypted_content,
    };
    let build = |turn: &CodexTurn, item: ProviderInputItem| {
        let mut input = input();
        input.push(item);
        CodexRequest::new(
            turn,
            "core",
            input,
            Vec::new(),
            request.limits,
            DataUseRestrictions::default(),
        )
    };
    assert!(build(&turn, continuation.clone()).is_ok());
    let other = provider.new_turn([1; 16], [2; 16], 1, "gpt-5.5").unwrap();
    assert!(build(&other, continuation.clone()).is_err());
    let ProviderInputItem::Reasoning {
        mut encrypted_content,
        id,
        summaries,
    } = continuation
    else {
        unreachable!()
    };
    encrypted_content.as_mut().unwrap().push('x');
    assert!(
        build(
            &turn,
            ProviderInputItem::Reasoning {
                id,
                summaries,
                encrypted_content
            }
        )
        .is_err()
    );
}
