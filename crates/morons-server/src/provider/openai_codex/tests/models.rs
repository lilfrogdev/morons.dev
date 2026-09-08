use super::*;

const EXPECTED: [(&str, &str); 6] = [
    ("gpt-5.5", "GPT-5.5 (ChatGPT)"),
    ("gpt-6-astra", "GPT-6 Astra (ChatGPT)"),
    ("gpt-5.6-sol", "GPT-5.6 Sol (ChatGPT)"),
    ("gpt-5.6-luna", "GPT-5.6 Luna (ChatGPT)"),
    ("gpt-5.6-terra", "GPT-5.6 Terra (ChatGPT)"),
    (
        "gpt-daybreak-blue-latest",
        "Daybreak Blue (ChatGPT; approval required)",
    ),
];

#[test]
fn requested_native_models_have_exact_full_responses_contracts_not_remote_admission() {
    assert_eq!(MODELS.len(), EXPECTED.len());
    let baseline: Value = serde_json::from_slice(&request(&turn()).body).unwrap();
    for (id, name) in EXPECTED {
        let t = CodexTurn::new(std::sync::Arc::new(()), [1; 16], [2; 16], 1, id).unwrap();
        let model = t.model();
        assert_eq!(model.display_name, name);
        assert_eq!(model.protocol_revision, 5);
        assert_eq!(
            (model.maximum_input_tokens, model.maximum_output_tokens),
            (96_000, 32_000)
        );
        assert_eq!(model.capabilities, MODELS[0].capabilities);
        assert_eq!(model.data_use, MODELS[0].data_use);
        for policy in [
            DataUseRestrictions {
                block_training_use: true,
                require_zero_retention: false,
            },
            DataUseRestrictions {
                block_training_use: false,
                require_zero_retention: true,
            },
            DataUseRestrictions {
                block_training_use: true,
                require_zero_retention: true,
            },
        ] {
            assert!(!policy.permits(model.data_use));
        }
        let mut expected = baseline.clone();
        expected["model"] = json!(id);
        let body: Value = serde_json::from_slice(&request(&t).body).unwrap();
        assert_eq!(body, expected, "only reviewed model identity changes");
        for key in [
            "access_programs",
            "service_tier",
            "max_output_tokens",
            "previous_response_id",
            "client_metadata",
        ] {
            assert!(body.get(key).is_none());
        }
        assert!(body["reasoning"].get("context").is_none());
        assert!(body.get("instructions").is_some());
        assert!(body.get("tools").is_some());
    }
    for id in [
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-7",
        "gpt-6-astra-pro",
        "gpt-daybreak-red-latest",
        "gpt-5.6-sol-latest",
    ] {
        assert!(matches!(
            CodexTurn::new(std::sync::Arc::new(()), [1; 16], [2; 16], 1, id),
            Err(ProviderError::UnsupportedModel)
        ));
    }
}
