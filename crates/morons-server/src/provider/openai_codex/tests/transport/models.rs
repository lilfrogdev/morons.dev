use super::*;

#[tokio::test(flavor = "current_thread")]
async fn new_native_model_rejection_or_alias_mismatch_never_falls_back_or_replays() {
    for (model, status, response_model) in [
        ("gpt-6-astra", "403 Forbidden", "gpt-6-astra"),
        ("gpt-daybreak-blue-latest", "200 OK", "gpt-5.6-sol"),
        ("gpt-5.6-terra", "401 Unauthorized", "gpt-5.6-terra"),
    ] {
        let (_root, _store, provider, listener) = setup().await;
        let peer = tokio::spawn(async move {
            let (mut socket, _) = time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let message = mock_request(&mut socket).await;
            let (headers, body) = message.split_once("\r\n\r\n").unwrap();
            assert!(!headers.to_lowercase().contains("responses-lite"));
            assert!(headers.contains("originator: morons"));
            let body: Value = serde_json::from_str(body).unwrap();
            assert_eq!(body["model"], model);
            assert!(body.get("access_programs").is_none());
            let response = String::from_utf8(sse(5))
                .unwrap()
                .replace("gpt-5.5", response_model);
            respond(&mut socket, status, "", response.as_bytes()).await;
            listener
        });
        let mut turn = provider.new_turn([1; 16], [2; 16], 1, model).unwrap();
        let first = request(&turn);
        let (_, mut cancel) = provider_cancellation();
        let result = provider
            .prepare_dispatch(
                &mut turn,
                &first,
                DataUseRestrictions::default(),
                &mut cancel,
            )
            .await
            .unwrap()
            .execute(DataUseRestrictions::default(), &mut cancel, |_| {})
            .await;
        assert!(result.is_err());
        assert!(!turn.usable);
        assert!(
            provider
                .prepare_dispatch(
                    &mut turn,
                    &first,
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
