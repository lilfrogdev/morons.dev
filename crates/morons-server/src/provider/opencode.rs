use std::{sync::Arc, time::Duration};

use super::response_http::*;
use bytes::{Bytes, BytesMut};
use http::{
    HeaderValue, Method, Request, Response, Uri,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use tokio::time::{self, Instant};
use zeroize::Zeroizing;

#[cfg(test)]
use super::responses::ResponsesDiagnostic;
use super::{
    OpenCodeCredentialLease, OpenCodeModelAvailability, OpenCodeResponseRequest, OpenCodeService,
    ProviderCancellation, ProviderError, ProviderOutcome, ProviderProtocol, ProviderStreamEvent,
    anthropic_messages::AnthropicMessagesDecoder,
    catalog::{MAX_CATALOG_BODY_BYTES, parse_catalog},
    chat_completions::ChatCompletionsDecoder,
    gemini::GeminiDecoder,
    http_client::{ProviderHttpClient, bounded_client},
    responses::ResponsesDecoder,
};
use crate::persistence::{PersistenceError, SessionStore};

const ZEN_INFERENCE_URI: &str = "https://opencode.ai/zen/v1/responses";
const ZEN_CHAT_INFERENCE_URI: &str = "https://opencode.ai/zen/v1/chat/completions";
const ZEN_ANTHROPIC_INFERENCE_URI: &str = "https://opencode.ai/zen/v1/messages";
const ZEN_GEMINI_INFERENCE_BASE: &str = "https://opencode.ai/zen/v1/models/";
const ZEN_CATALOG_URI: &str = "https://opencode.ai/zen/v1/models";
const GO_INFERENCE_URI: &str = "https://opencode.ai/zen/go/v1/responses";
const GO_CHAT_INFERENCE_URI: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const GO_ANTHROPIC_INFERENCE_URI: &str = "https://opencode.ai/zen/go/v1/messages";
const GO_CATALOG_URI: &str = "https://opencode.ai/zen/go/v1/models";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const USER_AGENT_VALUE: &str = concat!("morons-server/", env!("CARGO_PKG_VERSION"));
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";
const CATALOG_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct EndpointSet {
    zen_inference: String,
    zen_chat_inference: String,
    zen_anthropic_inference: String,
    zen_gemini_inference_base: String,
    zen_catalog: String,
    go_inference: String,
    go_chat_inference: String,
    go_anthropic_inference: String,
    go_catalog: String,
}

impl EndpointSet {
    fn production() -> Self {
        Self {
            zen_inference: ZEN_INFERENCE_URI.to_owned(),
            zen_chat_inference: ZEN_CHAT_INFERENCE_URI.to_owned(),
            zen_anthropic_inference: ZEN_ANTHROPIC_INFERENCE_URI.to_owned(),
            zen_gemini_inference_base: ZEN_GEMINI_INFERENCE_BASE.to_owned(),
            zen_catalog: ZEN_CATALOG_URI.to_owned(),
            go_inference: GO_INFERENCE_URI.to_owned(),
            go_chat_inference: GO_CHAT_INFERENCE_URI.to_owned(),
            go_anthropic_inference: GO_ANTHROPIC_INFERENCE_URI.to_owned(),
            go_catalog: GO_CATALOG_URI.to_owned(),
        }
    }

    fn inference(
        &self,
        service: OpenCodeService,
        protocol: ProviderProtocol,
        model_id: &str,
    ) -> Option<String> {
        match (service, protocol) {
            (OpenCodeService::Zen, ProviderProtocol::Responses) => Some(self.zen_inference.clone()),
            (OpenCodeService::Zen, ProviderProtocol::ChatCompletions) => {
                Some(self.zen_chat_inference.clone())
            }
            (OpenCodeService::Zen, ProviderProtocol::AnthropicMessages) => {
                Some(self.zen_anthropic_inference.clone())
            }
            (OpenCodeService::Zen, ProviderProtocol::Gemini) => Some(format!(
                "{}{model_id}:streamGenerateContent?alt=sse",
                self.zen_gemini_inference_base
            )),
            (OpenCodeService::Go, ProviderProtocol::Responses) => Some(self.go_inference.clone()),
            (OpenCodeService::Go, ProviderProtocol::ChatCompletions) => {
                Some(self.go_chat_inference.clone())
            }
            (OpenCodeService::Go, ProviderProtocol::AnthropicMessages) => {
                Some(self.go_anthropic_inference.clone())
            }
            (OpenCodeService::Go, ProviderProtocol::Gemini) => None,
        }
    }

    fn catalog(&self, service: OpenCodeService) -> &str {
        match service {
            OpenCodeService::Zen => &self.zen_catalog,
            OpenCodeService::Go => &self.go_catalog,
        }
    }
}

pub(crate) struct OpenCodeProvider {
    sessions: Arc<SessionStore>,
    client: OpenCodeClient,
}

pub(crate) struct PreparedOpenCodeDispatch<'a> {
    credential: OpenCodeCredentialLease<'a>,
    client: &'a OpenCodeClient,
    request: &'a OpenCodeResponseRequest,
    body: Bytes,
}

impl PreparedOpenCodeDispatch<'_> {
    pub(crate) async fn execute<F>(
        self,
        cancellation: &mut ProviderCancellation,
        on_event: F,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        F: FnMut(ProviderStreamEvent),
    {
        self.client
            .execute(
                self.credential,
                self.request,
                self.body,
                cancellation,
                on_event,
            )
            .await
    }
}

impl OpenCodeProvider {
    pub(crate) fn new(sessions: Arc<SessionStore>) -> Self {
        Self {
            sessions,
            client: OpenCodeClient::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(sessions: Arc<SessionStore>, base: &str) -> Self {
        let endpoints = EndpointSet {
            zen_inference: format!("{base}/zen/v1/responses"),
            zen_chat_inference: format!("{base}/zen/v1/chat/completions"),
            zen_anthropic_inference: format!("{base}/zen/v1/messages"),
            zen_gemini_inference_base: format!("{base}/zen/v1/models/"),
            zen_catalog: format!("{base}/zen/v1/models"),
            go_inference: format!("{base}/zen/go/v1/responses"),
            go_chat_inference: format!("{base}/zen/go/v1/chat/completions"),
            go_anthropic_inference: format!("{base}/zen/go/v1/messages"),
            go_catalog: format!("{base}/zen/go/v1/models"),
        };
        Self {
            sessions,
            client: OpenCodeClient::for_test(endpoints),
        }
    }

    pub(crate) async fn fetch_catalog(
        &self,
        service: OpenCodeService,
    ) -> Result<Vec<OpenCodeModelAvailability>, ProviderError> {
        self.client.fetch_catalog(service).await
    }

    pub(crate) async fn prepare_dispatch<'a>(
        &'a self,
        expected_credential_generation: u64,
        request: &'a OpenCodeResponseRequest,
    ) -> Result<PreparedOpenCodeDispatch<'a>, ProviderError> {
        let body = request.encoded_body();
        let credential = self
            .sessions
            .lease_open_code_credential(expected_credential_generation)
            .await
            .map_err(map_credential_error)?;
        Ok(PreparedOpenCodeDispatch {
            credential,
            client: &self.client,
            request,
            body,
        })
    }
}

struct OpenCodeClient {
    client: ProviderHttpClient,
    endpoints: EndpointSet,
    #[cfg(test)]
    emit_decoder_diagnostics: bool,
}

enum InferenceDecoder {
    Responses(ResponsesDecoder),
    ChatCompletions(ChatCompletionsDecoder),
    AnthropicMessages(AnthropicMessagesDecoder),
    Gemini(GeminiDecoder),
}

impl InferenceDecoder {
    fn new(request: &OpenCodeResponseRequest) -> Self {
        match request.model().protocol {
            ProviderProtocol::Responses => Self::Responses(ResponsesDecoder::new(
                request.model().id,
                request.model().maximum_input_tokens,
                request.model().maximum_output_tokens,
            )),
            ProviderProtocol::ChatCompletions => {
                Self::ChatCompletions(ChatCompletionsDecoder::new(
                    request.model().id,
                    request.model().maximum_input_tokens,
                    request.model().maximum_output_tokens,
                ))
            }
            ProviderProtocol::AnthropicMessages => {
                Self::AnthropicMessages(AnthropicMessagesDecoder::new(
                    request.model().id,
                    request.model().maximum_input_tokens,
                    request.model().maximum_output_tokens,
                ))
            }
            ProviderProtocol::Gemini => Self::Gemini(GeminiDecoder::new(
                request.model().maximum_input_tokens,
                request.model().maximum_output_tokens,
            )),
        }
    }

    fn push(&mut self, data: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        match self {
            Self::Responses(decoder) => decoder.push(data),
            Self::ChatCompletions(decoder) => decoder.push(data),
            Self::AnthropicMessages(decoder) => decoder.push(data),
            Self::Gemini(decoder) => decoder.push(data),
        }
    }

    fn finish(self) -> Result<ProviderOutcome, ProviderError> {
        match self {
            Self::Responses(decoder) => decoder.finish(),
            Self::ChatCompletions(decoder) => decoder.finish(),
            Self::AnthropicMessages(decoder) => decoder.finish(),
            Self::Gemini(decoder) => decoder.finish(),
        }
    }

    #[cfg(debug_assertions)]
    fn chat_diagnostic_stage(&self) -> Option<&'static str> {
        match self {
            Self::Responses(_) | Self::AnthropicMessages(_) | Self::Gemini(_) => None,
            Self::ChatCompletions(decoder) => Some(decoder.diagnostic_stage()),
        }
    }

    #[cfg(debug_assertions)]
    fn anthropic_diagnostic_stage(&self) -> Option<&'static str> {
        match self {
            Self::Responses(_) | Self::ChatCompletions(_) | Self::Gemini(_) => None,
            Self::AnthropicMessages(decoder) => Some(decoder.diagnostic_stage()),
        }
    }

    #[cfg(debug_assertions)]
    fn gemini_diagnostic_stage(&self) -> Option<&'static str> {
        match self {
            Self::Responses(_) | Self::ChatCompletions(_) | Self::AnthropicMessages(_) => None,
            Self::Gemini(decoder) => Some(decoder.diagnostic_stage()),
        }
    }

    #[cfg(test)]
    fn responses_diagnostic(&self) -> Option<ResponsesDiagnostic> {
        match self {
            Self::Responses(decoder) => Some(decoder.diagnostic()),
            Self::ChatCompletions(_) | Self::AnthropicMessages(_) | Self::Gemini(_) => None,
        }
    }
}

impl OpenCodeClient {
    fn new() -> Self {
        Self::build(EndpointSet::production(), false)
    }

    pub async fn fetch_catalog(
        &self,
        service: OpenCodeService,
    ) -> Result<Vec<OpenCodeModelAvailability>, ProviderError> {
        time::timeout(CATALOG_TOTAL_TIMEOUT, self.fetch_catalog_inner(service))
            .await
            .map_err(|_| ProviderError::TotalTimeout)?
    }

    async fn execute<F>(
        &self,
        credential: OpenCodeCredentialLease<'_>,
        request: &OpenCodeResponseRequest,
        body: Bytes,
        cancellation: &mut ProviderCancellation,
        on_event: F,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        F: FnMut(ProviderStreamEvent),
    {
        let response = self
            .send_inference(credential.api_key_bytes(), request, body, cancellation)
            .await;
        drop(credential);
        let (response, deadline) = response?;
        self.consume_inference(response, deadline, request, cancellation, on_event)
            .await
    }

    async fn fetch_catalog_inner(
        &self,
        service: OpenCodeService,
    ) -> Result<Vec<OpenCodeModelAvailability>, ProviderError> {
        let request = Request::builder()
            .method(Method::GET)
            .uri(parse_uri(self.endpoints.catalog(service))?)
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, USER_AGENT_VALUE)
            .body(Full::new(Bytes::new()))
            .map_err(|_| ProviderError::Transport)?;
        let response = time::timeout(RESPONSE_HEADER_TIMEOUT, self.client.request(request))
            .await
            .map_err(|_| ProviderError::ResponseHeaderTimeout)?
            .map_err(|_| ProviderError::Transport)?;
        validate_response_headers(response.headers())?;
        if !response.status().is_success() {
            return Err(classify_status(response.status()));
        }
        require_content_type(response.headers(), "application/json")?;
        validate_content_length(response.headers(), MAX_CATALOG_BODY_BYTES)?;
        let body = read_body_limited(
            response.into_body(),
            MAX_CATALOG_BODY_BYTES,
            STREAM_INACTIVITY_TIMEOUT,
        )
        .await?;
        parse_catalog(service, &body)
    }

    async fn send_inference(
        &self,
        api_key: &[u8],
        request: &OpenCodeResponseRequest,
        body: Bytes,
        cancellation: &mut ProviderCancellation,
    ) -> Result<(Response<Incoming>, Instant), ProviderError> {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let builder = Request::builder()
            .method(Method::POST)
            .uri(parse_uri(
                &self
                    .endpoints
                    .inference(
                        request.model().service,
                        request.model().protocol,
                        request.model().id,
                    )
                    .ok_or(ProviderError::UnsupportedModel)?,
            )?)
            .header(ACCEPT, "text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .header(OPENCODE_SESSION_HEADER, request.opencode_session_header())
            .header(USER_AGENT, USER_AGENT_VALUE);
        let builder = match request.model().protocol {
            ProviderProtocol::AnthropicMessages => builder
                .header("x-api-key", api_key_header(api_key)?)
                .header("anthropic-version", ANTHROPIC_VERSION),
            ProviderProtocol::Gemini => builder.header("x-goog-api-key", api_key_header(api_key)?),
            ProviderProtocol::Responses | ProviderProtocol::ChatCompletions => {
                builder.header(AUTHORIZATION, authorization_header(api_key)?)
            }
        };
        let http_request = builder
            .body(Full::new(body))
            .map_err(|_| ProviderError::Transport)?;
        let deadline = Instant::now() + PROVIDER_TOTAL_TIMEOUT;
        let response = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            result = time::timeout(RESPONSE_HEADER_TIMEOUT, self.client.request(http_request)) => {
                result
                    .map_err(|_| ProviderError::ResponseHeaderTimeout)?
                    .map_err(|_| ProviderError::Transport)?
            }
        };
        Ok((response, deadline))
    }

    async fn consume_inference<F>(
        &self,
        response: Response<Incoming>,
        deadline: Instant,
        request: &OpenCodeResponseRequest,
        cancellation: &mut ProviderCancellation,
        mut on_event: F,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        F: FnMut(ProviderStreamEvent),
    {
        validate_response_headers(response.headers())?;
        if !response.status().is_success() {
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
        require_content_type(response.headers(), "text/event-stream")?;
        let mut response_body = response.into_body();
        let mut decoder = InferenceDecoder::new(request);
        while let Some(frame) = next_frame(&mut response_body, deadline, cancellation).await? {
            let data = frame
                .into_data()
                .map_err(|_| ProviderError::MalformedResponse)?;
            let decoded = decoder.push(&data);
            if decoded.is_err() {
                #[cfg(debug_assertions)]
                if let Some(stage) = decoder.chat_diagnostic_stage() {
                    eprintln!("chat completions decoder rejected provider data while {stage}");
                }
                #[cfg(debug_assertions)]
                if let Some(stage) = decoder.anthropic_diagnostic_stage() {
                    eprintln!("Anthropic Messages decoder rejected provider data while {stage}");
                }
                #[cfg(debug_assertions)]
                if let Some(stage) = decoder.gemini_diagnostic_stage() {
                    eprintln!("Gemini decoder rejected provider data while {stage}");
                }
                #[cfg(test)]
                if self.emit_decoder_diagnostics
                    && let Some(diagnostic) = decoder.responses_diagnostic()
                {
                    emit_decoder_diagnostic(diagnostic);
                }
            }
            for event in decoded? {
                on_event(event);
            }
        }
        #[cfg(debug_assertions)]
        let chat_diagnostic_stage = decoder.chat_diagnostic_stage();
        #[cfg(debug_assertions)]
        let anthropic_diagnostic_stage = decoder.anthropic_diagnostic_stage();
        #[cfg(debug_assertions)]
        let gemini_diagnostic_stage = decoder.gemini_diagnostic_stage();
        #[cfg(test)]
        let diagnostic = self
            .emit_decoder_diagnostics
            .then(|| decoder.responses_diagnostic())
            .flatten();
        let outcome = decoder.finish();
        #[cfg(debug_assertions)]
        if outcome.is_err()
            && let Some(stage) = chat_diagnostic_stage
        {
            eprintln!("chat completions decoder could not finish after {stage}");
        }
        #[cfg(debug_assertions)]
        if outcome.is_err()
            && let Some(stage) = anthropic_diagnostic_stage
        {
            eprintln!("Anthropic Messages decoder could not finish after {stage}");
        }
        #[cfg(debug_assertions)]
        if outcome.is_err()
            && let Some(stage) = gemini_diagnostic_stage
        {
            eprintln!("Gemini decoder could not finish after {stage}");
        }
        #[cfg(test)]
        if outcome.is_err()
            && let Some(diagnostic) = diagnostic
        {
            emit_decoder_diagnostic(diagnostic);
        }
        outcome
    }

    #[cfg(test)]
    async fn execute_for_test<F>(
        &self,
        api_key: &[u8],
        request: &OpenCodeResponseRequest,
        cancellation: &mut ProviderCancellation,
        on_event: F,
    ) -> Result<ProviderOutcome, ProviderError>
    where
        F: FnMut(ProviderStreamEvent),
    {
        let body = request.encoded_body();
        let (response, deadline) = self
            .send_inference(api_key, request, body, cancellation)
            .await?;
        self.consume_inference(response, deadline, request, cancellation, on_event)
            .await
    }

    fn build(endpoints: EndpointSet, allow_http: bool) -> Self {
        Self {
            client: bounded_client(allow_http, None),
            endpoints,
            #[cfg(test)]
            emit_decoder_diagnostics: false,
        }
    }

    #[cfg(test)]
    fn for_test(endpoints: EndpointSet) -> Self {
        Self::build(endpoints, true)
    }

    #[cfg(test)]
    fn for_live_test() -> Self {
        let mut client = Self::new();
        client.emit_decoder_diagnostics = true;
        client
    }
}

#[cfg(test)]
fn emit_decoder_diagnostic(diagnostic: ResponsesDiagnostic) {
    eprintln!(
        "live provider decoder diagnostic: event_type={}, sequence_number={}, stage={}",
        diagnostic.event_type.as_deref().unwrap_or("unavailable"),
        diagnostic
            .sequence_number
            .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        diagnostic.stage
    );
}

fn map_credential_error(error: PersistenceError) -> ProviderError {
    match error {
        PersistenceError::CredentialGenerationConflict => {
            ProviderError::CredentialGenerationChanged
        }
        PersistenceError::CredentialNotConfigured => ProviderError::CredentialNotConfigured,
        _ => ProviderError::Transport,
    }
}

fn parse_uri(value: &str) -> Result<Uri, ProviderError> {
    value.parse().map_err(|_| ProviderError::Transport)
}

fn authorization_header(api_key: &[u8]) -> Result<HeaderValue, ProviderError> {
    let mut value = Zeroizing::new(Vec::with_capacity(7 + api_key.len()));
    value.extend_from_slice(b"Bearer ");
    value.extend_from_slice(api_key);
    sensitive_header(&value)
}

fn api_key_header(api_key: &[u8]) -> Result<HeaderValue, ProviderError> {
    sensitive_header(api_key)
}

fn sensitive_header(value: &[u8]) -> Result<HeaderValue, ProviderError> {
    let mut header = HeaderValue::from_bytes(value).map_err(|_| ProviderError::InvalidRequest)?;
    header.set_sensitive(true);
    Ok(header)
}

async fn read_body_limited(
    body: Incoming,
    maximum_bytes: usize,
    inactivity_timeout: Duration,
) -> Result<Vec<u8>, ProviderError> {
    let mut body = body;
    let mut output = BytesMut::new();
    while let Some(frame) = time::timeout(inactivity_timeout, body.frame())
        .await
        .map_err(|_| ProviderError::StreamInactivityTimeout)?
    {
        let frame = frame.map_err(|_| ProviderError::Transport)?;
        let data = frame
            .into_data()
            .map_err(|_| ProviderError::MalformedResponse)?;
        if output
            .len()
            .checked_add(data.len())
            .is_none_or(|length| length > maximum_bytes)
        {
            return Err(ProviderError::ResponseLimitExceeded);
        }
        output.extend_from_slice(&data);
    }
    Ok(output.to_vec())
}

#[cfg(test)]
mod tests;
