use super::{
    ModelCapabilities, ModelDataUse, OpenCodeModel, OpenCodeService, ProviderProtocol,
    find_open_code_model, openai_codex,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelService {
    Zen,
    Go,
    OpenAiChatGpt,
}

impl ModelService {
    pub const fn open_code(self) -> Option<OpenCodeService> {
        match self {
            Self::Zen => Some(OpenCodeService::Zen),
            Self::Go => Some(OpenCodeService::Go),
            Self::OpenAiChatGpt => None,
        }
    }
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Zen => "zen",
            Self::Go => "go",
            Self::OpenAiChatGpt => "openai-chatgpt",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::Zen => "OpenCode Zen",
            Self::Go => "OpenCode Go",
            Self::OpenAiChatGpt => "OpenAI ChatGPT",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelProfile {
    pub service: ModelService,
    pub id: &'static str,
    pub display_name: &'static str,
    pub protocol: ProviderProtocol,
    pub protocol_revision: u16,
    pub capabilities: ModelCapabilities,
    pub maximum_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub data_use: ModelDataUse,
    pub output_limit_is_local: bool,
}

impl From<OpenCodeModel> for ModelProfile {
    fn from(model: OpenCodeModel) -> Self {
        Self {
            service: match model.service {
                OpenCodeService::Zen => ModelService::Zen,
                OpenCodeService::Go => ModelService::Go,
            },
            id: model.id,
            display_name: model.display_name,
            protocol: model.protocol,
            protocol_revision: model.protocol_revision,
            capabilities: model.capabilities,
            maximum_input_tokens: model.maximum_input_tokens,
            maximum_output_tokens: model.maximum_output_tokens,
            data_use: model.data_use,
            output_limit_is_local: false,
        }
    }
}

pub fn find_model_profile(service: ModelService, id: &str) -> Option<ModelProfile> {
    if let Some(service) = service.open_code() {
        return find_open_code_model(service, id).copied().map(Into::into);
    }
    openai_codex::MODELS
        .iter()
        .find(|model| model.id == id)
        .map(|model| ModelProfile {
            service,
            id: model.id,
            display_name: model.display_name,
            protocol: ProviderProtocol::Responses,
            protocol_revision: model.protocol_revision,
            capabilities: model.capabilities,
            maximum_input_tokens: model.maximum_input_tokens,
            maximum_output_tokens: model.maximum_output_tokens,
            data_use: model.data_use,
            output_limit_is_local: true,
        })
}
