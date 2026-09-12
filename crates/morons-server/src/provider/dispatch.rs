use super::{
    DataUseRestrictions, ModelService, OpenCodeProvider, OpenCodeResponseRequest,
    PreparedProviderTools, ProviderCancellation, ProviderError, ProviderInputItem,
    ProviderMessageRole, ProviderOutcome, ProviderStreamEvent, find_model_profile,
    openai_codex::{
        CodexRequest, CodexRequestLimits, CodexTurn, OpenAiCodexProvider, PreparedCodexDispatch,
    },
};
use std::sync::Arc;
#[cfg(test)]
mod tests;

pub(crate) struct ModelProviders {
    pub open_code: Arc<OpenCodeProvider>,
    pub chatgpt: Arc<OpenAiCodexProvider>,
    pub credentials: Arc<super::openai_auth::OpenAiCredentialProvider>,
    registration: Arc<()>,
}
pub(crate) struct ModelTurn {
    service: ModelService,
    model: String,
    conversation: [u8; 16],
    generation: u64,
    native: Option<CodexTurn>,
    provider: Arc<()>,
    registration: Arc<()>,
}
pub(crate) struct ModelInput {
    pub input: Vec<ProviderInputItem>,
    pub estimated_input_tokens: u32,
    pub maximum_output_tokens: u32,
    pub tools: &'static PreparedProviderTools,
    /// True only for a Morons-created core instruction at index zero, never project guidance.
    pub core_first: bool,
}
pub(crate) enum ModelRequest {
    OpenCode {
        request: OpenCodeResponseRequest,
        registration: Arc<()>,
    },
    ChatGpt(CodexRequest),
}
pub(crate) enum ModelDispatch<'a> {
    OpenCode(super::opencode::PreparedOpenCodeDispatch<'a>),
    ChatGpt(PreparedCodexDispatch<'a>),
}

impl ModelProviders {
    pub(crate) fn new(
        sessions: Arc<crate::persistence::SessionStore>,
        open_code: Arc<OpenCodeProvider>,
    ) -> Arc<Self> {
        let credentials = Arc::new(super::openai_auth::OpenAiCredentialProvider::new(sessions));
        let chatgpt = Arc::new(OpenAiCodexProvider::new(credentials.clone()));
        Arc::new(Self {
            open_code,
            chatgpt,
            credentials,
            registration: Arc::new(()),
        })
    }
    #[cfg(test)]
    pub(crate) fn for_test(
        sessions: Arc<crate::persistence::SessionStore>,
        base: &str,
    ) -> Arc<Self> {
        let open_code = Arc::new(OpenCodeProvider::for_test(sessions.clone(), base));
        let credentials = Arc::new(super::openai_auth::OpenAiCredentialProvider::new(sessions));
        let chatgpt = Arc::new(OpenAiCodexProvider::for_test(
            credentials.clone(),
            format!("{base}/backend-api/codex/responses")
                .parse()
                .unwrap(),
        ));
        Arc::new(Self {
            open_code,
            chatgpt,
            credentials,
            registration: Arc::new(()),
        })
    }
    pub(crate) fn turn(
        &self,
        service: ModelService,
        model: &str,
        conversation: [u8; 16],
        run: [u8; 16],
        generation: u64,
    ) -> Result<ModelTurn, ProviderError> {
        if conversation == [0; 16]
            || run == [0; 16]
            || generation == 0
            || generation > i64::MAX as u64
        {
            return Err(ProviderError::InvalidRequest);
        }
        find_model_profile(service, model).ok_or(ProviderError::UnsupportedModel)?;
        let native = if service == ModelService::OpenAiChatGpt {
            Some(
                self.chatgpt
                    .new_turn(conversation, run, generation, model)?,
            )
        } else {
            None
        };
        Ok(ModelTurn {
            service,
            model: model.into(),
            conversation,
            generation,
            native,
            provider: self.registration.clone(),
            registration: Arc::new(()),
        })
    }
    pub(crate) async fn prepare_dispatch<'a>(
        &'a self,
        turn: &'a mut ModelTurn,
        request: &'a ModelRequest,
        policy: DataUseRestrictions,
        cancellation: &mut ProviderCancellation,
    ) -> Result<ModelDispatch<'a>, ProviderError> {
        if !Arc::ptr_eq(&turn.provider, &self.registration) {
            return Err(ProviderError::InvalidRequest);
        }
        let model =
            find_model_profile(turn.service, &turn.model).ok_or(ProviderError::UnsupportedModel)?;
        if !policy.permits(model.data_use) {
            return Err(ProviderError::DataUseRestricted);
        }
        match (turn.native.as_mut(), request) {
            (
                None,
                ModelRequest::OpenCode {
                    request,
                    registration,
                },
            ) if Arc::ptr_eq(registration, &turn.registration)
                && Some(request.model().service) == turn.service.open_code()
                && request.model().id == turn.model =>
            {
                self.open_code
                    .prepare_dispatch(turn.generation, request)
                    .await
                    .map(ModelDispatch::OpenCode)
            }
            (Some(native), ModelRequest::ChatGpt(request)) => self
                .chatgpt
                .prepare_dispatch(native, request, policy, cancellation)
                .await
                .map(ModelDispatch::ChatGpt),
            _ => Err(ProviderError::InvalidRequest),
        }
    }
}

impl ModelTurn {
    pub(crate) fn native_response_failure(
        &self,
    ) -> Option<super::response_diagnostic::ResponseStage> {
        self.native.as_ref().and_then(CodexTurn::response_failure)
    }
    pub(crate) fn request(&self, mut plan: ModelInput) -> Result<ModelRequest, ProviderError> {
        if let Some(native) = &self.native {
            let instructions = if plan.core_first {
                if plan.input.is_empty() {
                    return Err(ProviderError::InvalidRequest);
                }
                match plan.input.remove(0) {
                    ProviderInputItem::Message {
                        role: ProviderMessageRole::Developer,
                        text,
                        ..
                    } => text,
                    _ => return Err(ProviderError::InvalidRequest),
                }
            } else {
                crate::prompts::instruction(false).to_owned()
            };
            // Encoding grants no dispatch authority; the server checks policy before
            // credential preparation and again at durable dispatch admission.
            CodexRequest::with_prepared_tools(
                native,
                &instructions,
                &plan.input,
                plan.tools,
                CodexRequestLimits {
                    estimated_input_tokens: plan.estimated_input_tokens,
                    maximum_output_tokens: plan.maximum_output_tokens,
                },
                DataUseRestrictions::default(),
            )
            .map(ModelRequest::ChatGpt)
        } else {
            OpenCodeResponseRequest::with_prepared_tools(
                self.conversation,
                self.service
                    .open_code()
                    .ok_or(ProviderError::InvalidRequest)?,
                &self.model,
                plan.estimated_input_tokens,
                plan.maximum_output_tokens,
                plan.input,
                plan.tools,
            )
            .map(|request| ModelRequest::OpenCode {
                request,
                registration: self.registration.clone(),
            })
        }
    }
}
impl ModelDispatch<'_> {
    pub(crate) const fn diagnostic_attempt_id(&self) -> Option<u64> {
        match self {
            Self::OpenCode(dispatch) => dispatch.diagnostic_attempt_id(),
            Self::ChatGpt(_) => None,
        }
    }

    pub(crate) async fn execute<F: FnMut(ProviderStreamEvent)>(
        self,
        policy: DataUseRestrictions,
        cancellation: &mut ProviderCancellation,
        on_event: F,
    ) -> Result<ProviderOutcome, ProviderError> {
        match self {
            Self::OpenCode(dispatch) => dispatch.execute(cancellation, on_event).await,
            Self::ChatGpt(dispatch) => dispatch.execute(policy, cancellation, on_event).await,
        }
    }
}
