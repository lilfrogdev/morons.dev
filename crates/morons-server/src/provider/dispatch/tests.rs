use super::*;
use crate::persistence::{SessionStore, credential_tests::TestRoot};

#[tokio::test]
async fn prepared_requests_cannot_cross_provider_instances_or_turn_registrations() {
    let root = TestRoot::new("model-dispatch-scope");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    let provider = ModelProviders::for_test(store.clone(), "http://127.0.0.1:9");
    let other = ModelProviders::for_test(store, "http://127.0.0.1:9");
    for (service, model) in [
        (ModelService::Zen, "muse-spark-1.2"),
        (ModelService::OpenAiChatGpt, "gpt-5.5"),
    ] {
        let mut first = provider.turn(service, model, [1; 16], [2; 16], 1).unwrap();
        let mut second = provider.turn(service, model, [1; 16], [2; 16], 1).unwrap();
        let request = first
            .request(ModelInput {
                input: vec![ProviderInputItem::Message {
                    role: ProviderMessageRole::User,
                    text: "synthetic context".into(),
                    phase: None,
                }],
                estimated_input_tokens: 100,
                maximum_output_tokens: 16,
                tools: PreparedProviderTools::empty(),
                core_first: false,
            })
            .unwrap();
        let (_, mut cancellation) = super::super::provider_cancellation();
        assert!(matches!(
            other
                .prepare_dispatch(&mut first, &request, Default::default(), &mut cancellation)
                .await,
            Err(ProviderError::InvalidRequest)
        ));
        assert!(matches!(
            provider
                .prepare_dispatch(&mut second, &request, Default::default(), &mut cancellation)
                .await,
            Err(ProviderError::InvalidRequest)
        ));
    }
}
