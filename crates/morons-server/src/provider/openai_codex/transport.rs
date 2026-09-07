use super::{CodexRequest, CodexTurn, MODELS, OpenAiCodexModel};
use crate::{
    persistence::PersistenceError,
    provider::{
        DataUseRestrictions, ProviderCancellation, ProviderError, ProviderOutcome,
        ProviderStreamEvent,
        http_client::{ProviderHttpClient, bounded_client},
        openai_auth::{OpenAiCredentialError, OpenAiCredentialLease, OpenAiCredentialProvider},
        response_http::*,
        responses::ResponsesDecoder,
        sse::MAX_PROVIDER_STREAM_BYTES,
    },
};
use http::{
    Request, Uri,
    header::{ACCEPT, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::Full;
use std::sync::Arc;
use tokio::time::{self, Instant};

const ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const ROUTING_HEADER: &str = "x-codex-turn-state";
const MAX_ROUTING_BYTES: usize = 4096;
pub struct OpenAiCodexProvider {
    credentials: Arc<OpenAiCredentialProvider>,
    client: ProviderHttpClient,
    endpoint: Uri,
    registration: Arc<()>,
    header_timeout: std::time::Duration,
    total_timeout: std::time::Duration,
}
pub struct PreparedCodexDispatch<'a> {
    credential: OpenAiCredentialLease<'a>,
    provider: &'a OpenAiCodexProvider,
    request: &'a CodexRequest,
    turn: &'a mut CodexTurn,
}
impl OpenAiCodexProvider {
    pub(crate) fn new(credentials: Arc<OpenAiCredentialProvider>) -> Self {
        Self {
            credentials,
            client: bounded_client(
                false,
                Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
            ),
            endpoint: ENDPOINT.parse().expect("reviewed Codex route"),
            registration: Arc::new(()),
            header_timeout: RESPONSE_HEADER_TIMEOUT,
            total_timeout: PROVIDER_TOTAL_TIMEOUT,
        }
    }
    pub fn models(&self) -> &'static [OpenAiCodexModel] {
        MODELS
    }
    pub fn new_turn(
        &self,
        conversation: [u8; 16],
        run: [u8; 16],
        generation: u64,
        model: &str,
    ) -> Result<CodexTurn, ProviderError> {
        CodexTurn::new(
            self.registration.clone(),
            conversation,
            run,
            generation,
            model,
        )
    }
    pub async fn prepare_dispatch<'a>(
        &'a self,
        turn: &'a mut CodexTurn,
        request: &'a CodexRequest,
        policy: DataUseRestrictions,
        cancellation: &mut ProviderCancellation,
    ) -> Result<PreparedCodexDispatch<'a>, ProviderError> {
        if !Arc::ptr_eq(&turn.identity.provider, &self.registration) {
            return Err(ProviderError::InvalidRequest);
        }
        request.validate(turn, policy)?;
        let credential = self
            .credentials
            .lease(turn.identity.generation, cancellation)
            .await
            .map_err(credential_error)?;
        request.validate(turn, policy)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        Ok(PreparedCodexDispatch {
            credential,
            provider: self,
            request,
            turn,
        })
    }
    #[cfg(test)]
    pub(super) fn set_timeouts_for_test(
        &mut self,
        headers: std::time::Duration,
        total: std::time::Duration,
    ) {
        self.header_timeout = headers;
        self.total_timeout = total;
    }
    #[cfg(test)]
    pub(super) fn for_test(credentials: Arc<OpenAiCredentialProvider>, endpoint: Uri) -> Self {
        let mut provider = Self::new(credentials);
        provider.client = bounded_client(
            true,
            Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
        );
        provider.endpoint = endpoint;
        provider
    }
}
impl PreparedCodexDispatch<'_> {
    /// The caller must recheck the server's current policy and durable dispatch evidence first.
    pub async fn execute<F>(
        self,
        policy: DataUseRestrictions,
        cancellation: &mut ProviderCancellation,
        mut on_event: F,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        F: FnMut(ProviderStreamEvent),
    {
        self.request.validate(self.turn, policy)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let request_id = self.turn.request_id();
        self.turn.sequence = self
            .turn
            .sequence
            .checked_add(1)
            .ok_or(ProviderError::InvalidRequest)?;
        self.turn.usable = false;
        let mut request = Request::builder()
            .method("POST")
            .uri(self.provider.endpoint.clone())
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header(
                USER_AGENT,
                concat!("morons-server/", env!("CARGO_PKG_VERSION")),
            )
            .header("originator", "morons")
            .header("session-id", &self.turn.identity.session)
            .header("thread-id", &self.turn.identity.session)
            .header("x-client-request-id", request_id)
            .body(Full::new(self.request.body.clone()))
            .map_err(|_| ProviderError::InvalidRequest)?;
        request
            .headers_mut()
            .extend(self.credential.authorization_headers());
        if let Some(routing) = &self.turn.routing {
            request
                .headers_mut()
                .insert(ROUTING_HEADER, routing.clone());
        }
        let started = Instant::now();
        let deadline = started + self.provider.total_timeout;
        let header_deadline = (started + self.provider.header_timeout).min(deadline);
        let response = tokio::select! {
            biased;
            ()=cancellation.cancelled()=>return Err(ProviderError::Cancelled),
            result=time::timeout_at(header_deadline,self.provider.client.request(request))=>result.map_err(|_|if header_deadline==deadline {ProviderError::TotalTimeout}else{ProviderError::ResponseHeaderTimeout})?.map_err(|_|ProviderError::Transport)?,
        };
        drop(self.credential);
        validate_response_headers(response.headers())?;
        if response.status() != http::StatusCode::OK {
            let status = response.status();
            read_response_body_with_cancellation(
                response.into_body(),
                MAX_ERROR_BODY_BYTES,
                deadline,
                cancellation,
            )
            .await?;
            return Err(classify_status(status));
        }
        for name in [http::header::CONTENT_TYPE, http::header::CONTENT_LENGTH] {
            if response.headers().get_all(name).iter().nth(1).is_some() {
                return Err(ProviderError::MalformedResponse);
            }
        }
        require_content_type(response.headers(), "text/event-stream")?;
        validate_content_length(response.headers(), MAX_PROVIDER_STREAM_BYTES)?;
        let mut values = response.headers().get_all(ROUTING_HEADER).iter();
        let routing = values.next().cloned();
        if values.next().is_some() {
            return Err(ProviderError::MalformedResponse);
        }
        if let Some(mut routing) = routing {
            if routing.is_empty()
                || routing.as_bytes().len() > MAX_ROUTING_BYTES
                || !routing.as_bytes().iter().all(|b| matches!(b, 0x21..=0x7e))
            {
                return Err(ProviderError::MalformedResponse);
            }
            routing.set_sensitive(true);
            if self
                .turn
                .routing
                .as_ref()
                .is_some_and(|previous| previous != routing)
            {
                return Err(ProviderError::MalformedResponse);
            }
            self.turn.routing = Some(routing);
        }
        let mut decoder = ResponsesDecoder::new(
            self.request.identity.model.id,
            self.request.identity.model.maximum_input_tokens,
            self.request.limits.maximum_output_tokens,
        );
        let mut body = response.into_body();
        while let Some(frame) = next_frame(&mut body, deadline, cancellation).await? {
            let data = frame
                .into_data()
                .map_err(|_| ProviderError::MalformedResponse)?;
            for event in decoder.push(&data)? {
                on_event(event);
            }
        }
        let outcome = decoder.finish()?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ProviderError::TotalTimeout);
        }
        self.turn.reasoning = outcome
            .output
            .iter()
            .filter_map(|item| match item {
                crate::provider::ProviderOutputItem::Reasoning(reasoning) => {
                    Some(super::turn::reasoning_fingerprint(
                        &reasoning.provider_item_id,
                        &reasoning.summaries,
                        reasoning.encrypted_content.as_deref(),
                    ))
                }
                _ => None,
            })
            .collect();
        self.turn.usable = true;
        Ok(outcome)
    }
}
fn credential_error(error: OpenAiCredentialError) -> ProviderError {
    match error {
        OpenAiCredentialError::Cancelled => ProviderError::Cancelled,
        OpenAiCredentialError::Deadline => ProviderError::TotalTimeout,
        OpenAiCredentialError::Persistence(PersistenceError::CredentialGenerationConflict) => {
            ProviderError::CredentialGenerationChanged
        }
        OpenAiCredentialError::Persistence(
            PersistenceError::CredentialNotConfigured
            | PersistenceError::CredentialReauthenticationRequired,
        ) => ProviderError::CredentialNotConfigured,
        _ => ProviderError::Transport,
    }
}
