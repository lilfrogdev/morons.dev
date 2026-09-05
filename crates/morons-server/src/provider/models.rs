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
    AnthropicMessages,
    Gemini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelTrainingUse {
    NotUsed,
    MayUsePromptsAndCompletions,
    NotDocumented,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelRetention {
    None,
    UpToThirtyDays,
    NotZeroDataRetention,
    NotDocumented,
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
pub const ANTHROPIC_MESSAGES_PROTOCOL_REVISION: u16 = 3;
pub const GEMINI_PROTOCOL_REVISION: u16 = 4;
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
const VISION_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    image_input: true,
    ..RESPONSES_CAPABILITIES
};
const OPENAI_TEXT_RESPONSES_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    reasoning_continuation: true,
    ..RESPONSES_CAPABILITIES
};
const OPENAI_RESPONSES_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    image_input: true,
    ..OPENAI_TEXT_RESPONSES_CAPABILITIES
};
const GEMINI_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    reasoning_continuation: true,
    ..VISION_CAPABILITIES
};
const NO_TRAINING_ZERO_RETENTION: ModelDataUse = ModelDataUse {
    training: ModelTrainingUse::NotUsed,
    retention: ModelRetention::None,
};
const NO_TRAINING_THIRTY_DAY_RETENTION: ModelDataUse = ModelDataUse {
    training: ModelTrainingUse::NotUsed,
    retention: ModelRetention::UpToThirtyDays,
};
const TRAINING_NOT_ZERO_RETENTION: ModelDataUse = ModelDataUse {
    training: ModelTrainingUse::MayUsePromptsAndCompletions,
    retention: ModelRetention::NotZeroDataRetention,
};
const UNDOCUMENTED_DATA_USE: ModelDataUse = ModelDataUse {
    training: ModelTrainingUse::NotDocumented,
    retention: ModelRetention::NotDocumented,
};

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

const fn openai_text_model(
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
        OPENAI_TEXT_RESPONSES_CAPABILITIES,
    )
}

const fn vision_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    model_with_capabilities(service, id, display_name, data_use, VISION_CAPABILITIES)
}

const fn chat_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    protocol_model(
        service,
        id,
        display_name,
        data_use,
        RESPONSES_CAPABILITIES,
        ProviderProtocol::ChatCompletions,
        CHAT_COMPLETIONS_PROTOCOL_REVISION,
    )
}

const fn chat_vision_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    protocol_model(
        service,
        id,
        display_name,
        data_use,
        VISION_CAPABILITIES,
        ProviderProtocol::ChatCompletions,
        CHAT_COMPLETIONS_PROTOCOL_REVISION,
    )
}

const fn anthropic_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    protocol_model(
        service,
        id,
        display_name,
        data_use,
        RESPONSES_CAPABILITIES,
        ProviderProtocol::AnthropicMessages,
        ANTHROPIC_MESSAGES_PROTOCOL_REVISION,
    )
}

const fn anthropic_vision_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    protocol_model(
        service,
        id,
        display_name,
        data_use,
        VISION_CAPABILITIES,
        ProviderProtocol::AnthropicMessages,
        ANTHROPIC_MESSAGES_PROTOCOL_REVISION,
    )
}

const fn gemini_vision_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
) -> OpenCodeModel {
    protocol_model(
        service,
        id,
        display_name,
        data_use,
        GEMINI_CAPABILITIES,
        ProviderProtocol::Gemini,
        GEMINI_PROTOCOL_REVISION,
    )
}

const fn model_with_capabilities(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
    capabilities: ModelCapabilities,
) -> OpenCodeModel {
    protocol_model(
        service,
        id,
        display_name,
        data_use,
        capabilities,
        ProviderProtocol::Responses,
        RESPONSES_PROTOCOL_REVISION,
    )
}

const fn protocol_model(
    service: OpenCodeService,
    id: &'static str,
    display_name: &'static str,
    data_use: ModelDataUse,
    capabilities: ModelCapabilities,
    protocol: ProviderProtocol,
    protocol_revision: u16,
) -> OpenCodeModel {
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
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-fable-5",
        "Claude Fable 5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-fable-5-1",
        "Claude Fable 5.1",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-opus-5",
        "Claude Opus 5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-opus-4-8",
        "Claude Opus 4.8",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-opus-4-7",
        "Claude Opus 4.7",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-opus-4-6",
        "Claude Opus 4.6",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-opus-4-5",
        "Claude Opus 4.5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-sonnet-5",
        "Claude Sonnet 5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-sonnet-4-6",
        "Claude Sonnet 4.6",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-sonnet-4-5",
        "Claude Sonnet 4.5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-sonnet-4",
        "Claude Sonnet 4",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "claude-haiku-4-5",
        "Claude Haiku 4.5",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3.6-flash",
        "Gemini 3.6 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3.8-flash",
        "Gemini 3.8 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3.7-flash",
        "Gemini 3.7 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3.5-flash-lite",
        "Gemini 3.5 Flash Lite",
        NO_TRAINING_ZERO_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3.5-flash",
        "Gemini 3.5 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3.1-pro",
        "Gemini 3.1 Pro",
        NO_TRAINING_ZERO_RETENTION,
    ),
    gemini_vision_model(
        OpenCodeService::Zen,
        "gemini-3-flash",
        "Gemini 3 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
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
    openai_text_model(
        OpenCodeService::Zen,
        "gpt-5.3-codex-spark",
        "GPT 5.3 Codex Spark",
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
        "gpt-5.2",
        "GPT 5.2",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.2-codex",
        "GPT 5.2 Codex",
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
        "gpt-5.1-codex-max",
        "GPT 5.1 Codex Max",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.1-codex",
        "GPT 5.1 Codex",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5.1-codex-mini",
        "GPT 5.1 Codex Mini",
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
        "gpt-5-codex",
        "GPT 5 Codex",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Zen,
        "gpt-5-nano",
        "GPT 5 Nano",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    vision_model(
        OpenCodeService::Zen,
        "grok-build-0.1",
        "Grok Build 0.1",
        NO_TRAINING_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Zen,
        "grok-4.6",
        "Grok 4.6",
        NO_TRAINING_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Zen,
        "grok-4.5",
        "Grok 4.5",
        NO_TRAINING_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Zen,
        "muse-spark-1.2",
        "Muse Spark 1.2",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "deepseek-v4-pro",
        "DeepSeek V4 Pro",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "deepseek-v4-flash",
        "DeepSeek V4 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "glm-5.2",
        "GLM-5.2",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "glm-5.1",
        "GLM-5.1",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "glm-5",
        "GLM-5",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Zen,
        "minimax-m3",
        "MiniMax M3",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "minimax-m2.7",
        "MiniMax M2.7",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "minimax-m2.5",
        "MiniMax M2.5",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Zen,
        "kimi-k3",
        "Kimi K3",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Zen,
        "kimi-k2.7-code",
        "Kimi K2.7 Code",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Zen,
        "kimi-k2.6",
        "Kimi K2.6",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Zen,
        "kimi-k2.5",
        "Kimi K2.5",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "qwen3.6-plus",
        "Qwen3.6 Plus",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Zen,
        "qwen3.5-plus",
        "Qwen3.5 Plus",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "big-pickle",
        "Big Pickle",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "deepseek-v4-flash-free",
        "DeepSeek V4 Flash Free",
        NO_TRAINING_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Zen,
        "muse-spark-1.3-contributor-free",
        "Muse Spark 1.3 Contributor Free",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Zen,
        "muse-spark-1.2-contributor-free",
        "Muse Spark 1.2 Contributor Free",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Zen,
        "mimo-v2.5-free",
        "MiMo V2.5 Free",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "ling-3.0-flash-fin-free",
        "Ling 3.0 Flash Fin Free",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "nemotron-3-ultra-free",
        "Nemotron 3 Ultra Free",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "nemotron-3.5-lightning-free",
        "Nemotron 3.5 Lightning Free",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Zen,
        "laguna-s-2.1-free",
        "Laguna S 2.1 Free",
        NO_TRAINING_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Go,
        "grok-4.6",
        "Grok 4.6",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    openai_model(
        OpenCodeService::Go,
        "gpt-5.6-luna",
        "GPT 5.6 Luna",
        NO_TRAINING_THIRTY_DAY_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "glm-5.3-flash",
        "GLM-5.3-Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "glm-5.3",
        "GLM-5.3",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "glm-5.2",
        "GLM-5.2",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "glm-5.1",
        "GLM-5.1",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "kimi-k3",
        "Kimi K3",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "kimi-k2.7-code",
        "Kimi K2.7 Code",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "kimi-k2.6",
        "Kimi K2.6",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "longcat-2.0",
        "LongCat-2.0",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "mimo-v2.5",
        "MiMo V2.5",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "mimo-v2.5-pro",
        "MiMo V2.5 Pro",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_model(
        OpenCodeService::Go,
        "minimax-m3",
        "MiniMax M3",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_model(
        OpenCodeService::Go,
        "minimax-m2.7",
        "MiniMax M2.7",
        NO_TRAINING_ZERO_RETENTION,
    ),
    // The current endpoint table still pins this route, but the current model and
    // privacy tables omit M2.5, so its data-use policy remains undisclosed.
    anthropic_model(
        OpenCodeService::Go,
        "minimax-m2.5",
        "MiniMax M2.5",
        UNDOCUMENTED_DATA_USE,
    ),
    vision_model(
        OpenCodeService::Go,
        "muse-spark-1.3-contributor",
        "Muse Spark 1.3 Contributor",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    vision_model(
        OpenCodeService::Go,
        "muse-spark-1.2-contributor",
        "Muse Spark 1.2 Contributor",
        TRAINING_NOT_ZERO_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Go,
        "qwen3.8-max",
        "Qwen3.8 Max",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Go,
        "qwen3.8-flash",
        "Qwen3.8 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_model(
        OpenCodeService::Go,
        "qwen3.7-max",
        "Qwen3.7 Max",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Go,
        "qwen3.7-plus",
        "Qwen3.7 Plus",
        NO_TRAINING_ZERO_RETENTION,
    ),
    anthropic_vision_model(
        OpenCodeService::Go,
        "qwen3.6-plus",
        "Qwen3.6 Plus",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "deepseek-v4-pro",
        "DeepSeek V4 Pro",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "deepseek-v4-flash",
        "DeepSeek V4 Flash",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "deepseek-v4-flash-vision-exp",
        "DeepSeek V4 Flash Vision Exp",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "hy4-preview",
        "Hy4 preview",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_model(
        OpenCodeService::Go,
        "hy3",
        "Hy3",
        NO_TRAINING_ZERO_RETENTION,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "omen-alpha",
        "Omen Alpha",
        NO_TRAINING_ZERO_RETENTION,
    ),
    // These identifiers remain in Go's public catalog but no longer appear in the
    // current endpoint/privacy tables. Keep their routing reviewed and disclose
    // their data-use classification as undocumented rather than guessing.
    vision_model(
        OpenCodeService::Go,
        "grok-4.5",
        "Grok 4.5",
        UNDOCUMENTED_DATA_USE,
    ),
    chat_model(OpenCodeService::Go, "glm-5", "GLM-5", UNDOCUMENTED_DATA_USE),
    chat_vision_model(
        OpenCodeService::Go,
        "kimi-k2.5",
        "Kimi K2.5",
        UNDOCUMENTED_DATA_USE,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "qwen3.5-plus",
        "Qwen3.5 Plus",
        UNDOCUMENTED_DATA_USE,
    ),
    chat_model(
        OpenCodeService::Go,
        "mimo-v2-pro",
        "MiMo V2 Pro",
        UNDOCUMENTED_DATA_USE,
    ),
    chat_vision_model(
        OpenCodeService::Go,
        "mimo-v2-omni",
        "MiMo V2 Omni",
        UNDOCUMENTED_DATA_USE,
    ),
    chat_model(
        OpenCodeService::Go,
        "hy3-preview",
        "Hy3 Preview",
        UNDOCUMENTED_DATA_USE,
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
        ANTHROPIC_MESSAGES_PROTOCOL_REVISION, CHAT_COMPLETIONS_PROTOCOL_REVISION,
        GEMINI_PROTOCOL_REVISION, MAXIMUM_CONTEXT_TOKENS, ModelRetention, ModelTrainingUse,
        OpenCodeService, ProviderProtocol, RESPONSES_PROTOCOL_REVISION, find_open_code_model,
        open_code_models,
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
            assert_eq!(
                model.protocol_revision,
                match model.protocol {
                    ProviderProtocol::Responses => RESPONSES_PROTOCOL_REVISION,
                    ProviderProtocol::ChatCompletions => CHAT_COMPLETIONS_PROTOCOL_REVISION,
                    ProviderProtocol::AnthropicMessages => ANTHROPIC_MESSAGES_PROTOCOL_REVISION,
                    ProviderProtocol::Gemini => GEMINI_PROTOCOL_REVISION,
                }
            );
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
                && model.capabilities.image_input)
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
                .is_some_and(|model| model.capabilities.image_input)
        );
        assert!(
            find_open_code_model(OpenCodeService::Zen, "gpt-5.3-codex-spark")
                .is_some_and(|model| !model.capabilities.image_input)
        );
        assert!(find_open_code_model(OpenCodeService::Zen, "Gpt-5.6-luna").is_none());
        assert!(
            find_open_code_model(OpenCodeService::Zen, "gemini-3.8-flash")
                .is_some_and(|model| model.protocol == ProviderProtocol::Gemini)
        );
        assert!(
            find_open_code_model(OpenCodeService::Zen, "claude-sonnet-5")
                .is_some_and(|model| model.protocol == ProviderProtocol::AnthropicMessages)
        );
        assert!(
            find_open_code_model(OpenCodeService::Zen, "big-pickle")
                .is_some_and(|model| model.protocol == ProviderProtocol::ChatCompletions)
        );
        let contributor = find_open_code_model(OpenCodeService::Go, "muse-spark-1.2-contributor")
            .expect("current Go contributor model should be reviewed");
        assert_eq!(
            contributor.data_use.training,
            ModelTrainingUse::MayUsePromptsAndCompletions
        );
        assert_eq!(
            contributor.data_use.retention,
            ModelRetention::NotZeroDataRetention
        );
        assert!(
            find_open_code_model(OpenCodeService::Go, "qwen3.8-max")
                .is_some_and(|model| model.protocol == ProviderProtocol::AnthropicMessages)
        );
        assert!(find_open_code_model(OpenCodeService::Go, "ox-alpha-free").is_none());
    }

    #[test]
    fn reviewed_zen_manifest_covers_the_complete_live_catalog_snapshot() {
        let actual = open_code_models()
            .iter()
            .filter(|model| model.service == OpenCodeService::Zen)
            .map(|model| model.id)
            .collect::<BTreeSet<_>>();
        let expected = [
            "big-pickle",
            "claude-fable-5",
            "claude-fable-5-1",
            "claude-haiku-4-5",
            "claude-opus-4-5",
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-sonnet-4",
            "claude-sonnet-4-5",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
            "deepseek-v4-flash",
            "deepseek-v4-flash-free",
            "deepseek-v4-pro",
            "gemini-3-flash",
            "gemini-3.1-pro",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.6-flash",
            "gemini-3.7-flash",
            "gemini-3.8-flash",
            "glm-5",
            "glm-5.1",
            "glm-5.2",
            "gpt-5",
            "gpt-5-codex",
            "gpt-5-nano",
            "gpt-5.1",
            "gpt-5.1-codex",
            "gpt-5.1-codex-max",
            "gpt-5.1-codex-mini",
            "gpt-5.2",
            "gpt-5.2-codex",
            "gpt-5.3-codex",
            "gpt-5.3-codex-spark",
            "gpt-5.4",
            "gpt-5.4-mini",
            "gpt-5.4-nano",
            "gpt-5.4-pro",
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.6-luna",
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "grok-4.5",
            "grok-4.6",
            "grok-build-0.1",
            "kimi-k2.5",
            "kimi-k2.6",
            "kimi-k2.7-code",
            "kimi-k3",
            "laguna-s-2.1-free",
            "ling-3.0-flash-fin-free",
            "mimo-v2.5-free",
            "minimax-m2.5",
            "minimax-m2.7",
            "minimax-m3",
            "muse-spark-1.2",
            "muse-spark-1.2-contributor-free",
            "muse-spark-1.3-contributor-free",
            "nemotron-3-ultra-free",
            "nemotron-3.5-lightning-free",
            "qwen3.5-plus",
            "qwen3.6-plus",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);

        let protocols = |protocol| {
            open_code_models()
                .iter()
                .filter(|model| model.service == OpenCodeService::Zen && model.protocol == protocol)
                .map(|model| model.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            protocols(ProviderProtocol::Responses),
            [
                "gpt-5",
                "gpt-5-codex",
                "gpt-5-nano",
                "gpt-5.1",
                "gpt-5.1-codex",
                "gpt-5.1-codex-max",
                "gpt-5.1-codex-mini",
                "gpt-5.2",
                "gpt-5.2-codex",
                "gpt-5.3-codex",
                "gpt-5.3-codex-spark",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.4-nano",
                "gpt-5.4-pro",
                "gpt-5.5",
                "gpt-5.5-pro",
                "gpt-5.6-luna",
                "gpt-5.6-sol",
                "gpt-5.6-terra",
                "grok-4.5",
                "grok-4.6",
                "grok-build-0.1",
                "muse-spark-1.2",
                "muse-spark-1.2-contributor-free",
                "muse-spark-1.3-contributor-free",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            protocols(ProviderProtocol::ChatCompletions),
            [
                "big-pickle",
                "deepseek-v4-flash",
                "deepseek-v4-flash-free",
                "deepseek-v4-pro",
                "glm-5",
                "glm-5.1",
                "glm-5.2",
                "kimi-k2.5",
                "kimi-k2.6",
                "kimi-k2.7-code",
                "kimi-k3",
                "laguna-s-2.1-free",
                "ling-3.0-flash-fin-free",
                "mimo-v2.5-free",
                "minimax-m2.5",
                "minimax-m2.7",
                "minimax-m3",
                "nemotron-3-ultra-free",
                "nemotron-3.5-lightning-free",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            protocols(ProviderProtocol::AnthropicMessages),
            [
                "claude-fable-5",
                "claude-fable-5-1",
                "claude-haiku-4-5",
                "claude-opus-4-5",
                "claude-opus-4-6",
                "claude-opus-4-7",
                "claude-opus-4-8",
                "claude-opus-5",
                "claude-sonnet-4",
                "claude-sonnet-4-5",
                "claude-sonnet-4-6",
                "claude-sonnet-5",
                "qwen3.5-plus",
                "qwen3.6-plus",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            protocols(ProviderProtocol::Gemini),
            [
                "gemini-3-flash",
                "gemini-3.1-pro",
                "gemini-3.5-flash",
                "gemini-3.5-flash-lite",
                "gemini-3.6-flash",
                "gemini-3.7-flash",
                "gemini-3.8-flash",
            ]
            .into_iter()
            .collect()
        );

        let training_eligible = open_code_models()
            .iter()
            .filter(|model| {
                model.service == OpenCodeService::Zen
                    && model.data_use.training == ModelTrainingUse::MayUsePromptsAndCompletions
            })
            .map(|model| model.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            training_eligible,
            [
                "big-pickle",
                "ling-3.0-flash-fin-free",
                "mimo-v2.5-free",
                "muse-spark-1.2-contributor-free",
                "muse-spark-1.3-contributor-free",
                "nemotron-3-ultra-free",
                "nemotron-3.5-lightning-free",
            ]
            .into_iter()
            .collect()
        );
        assert!(training_eligible.iter().all(|model_id| {
            find_open_code_model(OpenCodeService::Zen, model_id).is_some_and(|model| {
                model.data_use.retention == ModelRetention::NotZeroDataRetention
            })
        }));
        assert_eq!(
            find_open_code_model(OpenCodeService::Zen, "gpt-5.6-luna")
                .expect("Zen GPT should be reviewed")
                .data_use
                .retention,
            ModelRetention::UpToThirtyDays
        );
        assert_eq!(
            find_open_code_model(OpenCodeService::Zen, "claude-sonnet-5")
                .expect("Zen Claude should be reviewed")
                .data_use
                .retention,
            ModelRetention::UpToThirtyDays
        );
        assert_eq!(
            find_open_code_model(OpenCodeService::Zen, "gemini-3.8-flash")
                .expect("Zen Gemini should be reviewed")
                .data_use
                .retention,
            ModelRetention::None
        );
    }

    #[test]
    fn reviewed_go_manifest_covers_the_complete_live_catalog_snapshot() {
        let actual = open_code_models()
            .iter()
            .filter(|model| model.service == OpenCodeService::Go)
            .map(|model| model.id)
            .collect::<BTreeSet<_>>();
        let expected = [
            "deepseek-v4-flash",
            "deepseek-v4-flash-vision-exp",
            "deepseek-v4-pro",
            "glm-5",
            "glm-5.1",
            "glm-5.2",
            "glm-5.3",
            "glm-5.3-flash",
            "gpt-5.6-luna",
            "grok-4.5",
            "grok-4.6",
            "hy3",
            "hy3-preview",
            "hy4-preview",
            "kimi-k2.5",
            "kimi-k2.6",
            "kimi-k2.7-code",
            "kimi-k3",
            "longcat-2.0",
            "mimo-v2-omni",
            "mimo-v2-pro",
            "mimo-v2.5",
            "mimo-v2.5-pro",
            "minimax-m2.5",
            "minimax-m2.7",
            "minimax-m3",
            "muse-spark-1.2-contributor",
            "muse-spark-1.3-contributor",
            "omen-alpha",
            "qwen3.5-plus",
            "qwen3.6-plus",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.8-flash",
            "qwen3.8-max",
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);

        let undocumented_data_use = open_code_models()
            .iter()
            .filter(|model| {
                model.service == OpenCodeService::Go
                    && (model.data_use.training == ModelTrainingUse::NotDocumented
                        || model.data_use.retention == ModelRetention::NotDocumented)
            })
            .map(|model| model.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            undocumented_data_use,
            [
                "glm-5",
                "grok-4.5",
                "hy3-preview",
                "kimi-k2.5",
                "mimo-v2-omni",
                "mimo-v2-pro",
                "minimax-m2.5",
                "qwen3.5-plus",
            ]
            .into_iter()
            .collect()
        );

        let protocols = |protocol| {
            open_code_models()
                .iter()
                .filter(|model| model.service == OpenCodeService::Go && model.protocol == protocol)
                .map(|model| model.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            protocols(ProviderProtocol::Responses),
            [
                "gpt-5.6-luna",
                "grok-4.5",
                "grok-4.6",
                "muse-spark-1.2-contributor",
                "muse-spark-1.3-contributor",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(
            protocols(ProviderProtocol::AnthropicMessages),
            [
                "minimax-m2.5",
                "minimax-m2.7",
                "minimax-m3",
                "qwen3.6-plus",
                "qwen3.7-max",
                "qwen3.7-plus",
                "qwen3.8-flash",
                "qwen3.8-max",
            ]
            .into_iter()
            .collect()
        );
        assert_eq!(protocols(ProviderProtocol::ChatCompletions).len(), 22);
    }
}
