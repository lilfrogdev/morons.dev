#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenCodeService {
    Zen,
    Go,
}

impl OpenCodeService {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zen => "zen",
            Self::Go => "go",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderProtocol {
    Responses,
    ChatCompletions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelTrainingUse {
    NotUsed,
    MayUsePromptsAndCompletions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRetention {
    None,
    UpToThirtyDays,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelDataUse {
    pub training: ModelTrainingUse,
    pub retention: ModelRetention,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub text_input: bool,
    pub image_input: bool,
    pub text_output: bool,
    pub reasoning: bool,
    pub reasoning_continuation: bool,
    pub tool_calls: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenCodeModel {
    pub service: OpenCodeService,
    pub id: &'static str,
    pub display_name: &'static str,
    pub protocol: ProviderProtocol,
    pub protocol_revision: u16,
    pub capabilities: ModelCapabilities,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub data_use: ModelDataUse,
}

pub const RESPONSES_PROTOCOL_REVISION: u16 = 1;
pub const CHAT_COMPLETIONS_PROTOCOL_REVISION: u16 = 2;
pub const MAXIMUM_INPUT_TOKENS: u32 = 96_000;
pub const MAXIMUM_OUTPUT_TOKENS: u32 = 32_000;
pub const MAXIMUM_CONTEXT_TOKENS: u32 = MAXIMUM_INPUT_TOKENS + MAXIMUM_OUTPUT_TOKENS;

const RESPONSES_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    text_input: true,
    image_input: false,
    text_output: true,
    reasoning: true,
    reasoning_continuation: false,
    tool_calls: true,
};
const OPENAI_RESPONSES_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    image_input: true,
    reasoning_continuation: true,
    ..RESPONSES_CAPABILITIES
};
const NO_TRAINING_ZERO_RETENTION: ModelDataUse = ModelDataUse {
    training: ModelTrainingUse::NotUsed,
    retention: ModelRetention::None,
};
const NO_TRAINING_THIRTY_DAY_RETENTION: ModelDataUse = ModelDataUse {
    training: ModelTrainingUse::NotUsed,
    retention: ModelRetention::UpToThirtyDays,
};

const fn model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    model_with_capabilities(service, id, display_name, data_use, RESPONSES_CAPABILITIES)
}

const fn openai_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    model_with_capabilities(
        service,
        id,
        display_name,
        data_use,
        OPENAI_RESPONSES_CAPABILITIES,
    )
}

const fn chat_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    model_with_protocol(
        service,
        id,
        display_name,
        data_use,
        RESPONSES_CAPABILITIES,
        ProviderProtocol::ChatCompletions,
        CHAT_COMPLETIONS_PROTOCOL_REVISION,
    )
}

const fn model_with_capabilities(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
    capabilities: ModelCapabilities,
) -> OpenCodeModel {
    model_with_protocol(
        service,
        id,
        display_name,
        data_use,
        capabilities,
        ProviderProtocol::Responses,
        RESPONSES_PROTOCOL_REVISION,
    )
}

const fn model_with_protocol(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
    capabilities: ModelCapabilities,
    protocol: ProviderProtocol,
    protocol_revision: u16,
) -> OpenCodeModel {
    assert!(matches!(data_use.training, ModelTrainingUse::NotUsed));
    OpenCodeModel {
        service,
        id,
        display_name,
        protocol,
        protocol_revision,
        capabilities,
        maximum_input_tokens: MAXIMUM_INPUT_TOKENS,
        maximum_output_tokens: MAXIMUM_OUTPUT_TOKENS,
        data_use,
    }
}

static MODELS: &[OpenCodeModel] = &[
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.6-sol",
        "GPT 5.6 Sol",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.6-terra",
        "GPT 5.6 Terra",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.6-luna",
        "GPT 5.6 Luna",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.5",
        "GPT 5.5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.5-pro",
        "GPT 5.5 Pro",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.4",
        "GPT 5.4",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.4-pro",
        "GPT 5.4 Pro",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.4-mini",
        "GPT 5.4 Mini",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.4-nano",
        "GPT 5.4 Nano",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.3-codex",
        "GPT 5.3 Codex",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.3-codex-spark",
        "GPT 5.3 Codex Spark",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.2",
        "GPT 5.2",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.1",
        "GPT 5.1",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5",
        "GPT 5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5-nano",
        "GPT 5 Nano",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    model(
        OpenCodeService::Zen,
        "grok-4.6",
        "Grok 4.6",
        NO_TRAINING_ZERO_RETENTION,
    ),
    model(
        OpenCodeService::Zen,
        "grok-4.5",
        "Grok 4.5",
        NO_TRAINING_ZERO_RETENTION,
    ),
    model(
        OpenCodeService::Zen,
        "grok-build-0.1",
        "Grok Build 0.1",
        NO_TRAINING_ZERO_RETENTION,
    ),
    model(
        OpenCodeService::Zen,
        "muse-spark-1.2",
        "Muse Spark 1.2",
        NO_TRAINING_ZERO_RETENTION,
    ),
    openai_model(
        OpenCodeService::Go,
        "gpt-5.6-luna",
        "GPT 5.6 Luna",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    model(
        OpenCodeService::Go,
        "grok-4.6",
        "Grok 4.6",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "glm-5.3-flash",
        "GLM-5.3-Flash",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
];

#[must_use]
pub fn open_code_models() -> &'static [OpenCodeModel] {
    MODELS
}

#[must_use]
pub fn find_open_code_model(
    service: OpenCodeService,
    model_id: &str,
) -> Option<&'static OpenCodeModel> {
    MODELS
        .iter()
        .find(|model| model.service == service && model.id == model_id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        MAXIMUM_CONTEXT_TOKENS, ModelTrainingUse, OpenCodeService, ProviderProtocol,
        find_open_code_model, open_code_models,
    };

    #[test]
    fn manifest_has_unique_bounded_service_model_pairs() {
        let mut pairs = BTreeSet::new();
        for model in open_code_models() {
            assert!(!model.id.is_empty() && model.id.len() <= 128);
            assert!(model.id.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'-' | b'_')));
            assert!(!model.display_name.is_empty() && model.display_name.len() <= 128);
            assert_eq!(
                model.maximum_input_tokens + model.maximum_output_tokens,
                MAXIMUM_CONTEXT_TOKENS
            );
            assert_eq!(model.data_use.training, ModelTrainingUse::NotUsed);
            assert!(pairs.insert((model.service.as_str(), model.id)));
        }
    }

    #[test]
    fn lookup_requires_an_exact_reviewed_service_and_model_pair() {
        assert!(find_open_code_model(OpenCodeService::Zen, "gpt-5.6-luna").is_some());
        assert!(find_open_code_model(OpenCodeService::Go, "gpt-5.6-luna").is_some());
        assert!(find_open_code_model(OpenCodeService::Go, "gpt-5.6-sol").is_none());
        assert!(
            find_open_code_model(OpenCodeService::Go, "glm-5.3-flash").is_some_and(|model| model
                .protocol
                == ProviderProtocol::ChatCompletions
                && model.capabilities.tool_calls
                && !model.capabilities.image_input)
        );
        assert_eq!(
            find_open_code_model(OpenCodeService::Zen, "grok-4.6")
                .expect("Zen Grok should be reviewed")
                .data_use
                .retention,
            super::ModelRetention::None
        );
        assert_eq!(
            find_open_code_model(OpenCodeService::Go, "grok-4.6")
                .expect("Go Grok should be reviewed")
                .data_use
                .retention,
            super::ModelRetention::UpToThirtyDays
        );
        assert!(
            find_open_code_model(OpenCodeService::Zen, "gpt-5.4")
                .is_some_and(|model| model.capabilities.image_input)
        );
        assert!(
            find_open_code_model(OpenCodeService::Zen, "muse-spark-1.2")
                .is_some_and(|model| !model.capabilities.image_input)
        );
        assert!(find_open_code_model(OpenCodeService::Zen, "Gpt-5.6-luna").is_none());
        for model_id in [
            "big-pickle",
            "mimo-v2.5-free",
            "ling-3.0-flash-fin-free",
            "nemotron-3-ultra-free",
            "nemotron-3.5-lightning-free",
            "muse-spark-1.2-contributor-free",
        ] {
            assert!(find_open_code_model(OpenCodeService::Zen, model_id).is_none());
        }
        assert!(find_open_code_model(OpenCodeService::Go, "muse-spark-1.2-contributor").is_none());
    }
}
