use std::{sync::Arc, time::Duration};

use bytes::{Bytes, BytesMut};
use http::{
    HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri,
    header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Frame, Incoming};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use tokio::time::{self, Instant};
use zeroize::Zeroizing;

#[cfg(test)]
use super::responses::ResponsesDiagnostic;
use super::{
    OpenCodeCredentialLease, OpenCodeModelAvailability, OpenCodeResponseRequest, OpenCodeService,
    ProviderCancellation, ProviderError, ProviderOutcome, ProviderProtocol, ProviderStreamEvent,
    catalog::{MAX_CATALOG_BODY_BYTES, parse_catalog},
    chat_completions::ChatCompletionsDecoder,
    responses::ResponsesDecoder,
};
use crate::persistence::{PersistenceError, SessionStore};

const ZEN_INFERENCE_URI: &str = "https://opencode.ai/zen/v1/responses";
const ZEN_CATALOG_URI: &str = "https://opencode.ai/zen/v1/models";
const GO_INFERENCE_URI: &str = "https://opencode.ai/zen/go/v1/responses";
const GO_CHAT_INFERENCE_URI: &str = "https://opencode.ai/zen/go/v1/chat/completions";
const GO_CATALOG_URI: &str = "https://opencode.ai/zen/go/v1/models";
const USER_AGENT_VALUE: &str = concat!("morons-server/", env!("CARGO_PKG_VERSION"));
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_TOTAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CATALOG_TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_HEADERS: usize = 64;
const MAX_RESPONSE_HEADER_BYTES: usize = 16 * 1024;
const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

type ProviderHttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

#[derive(Clone)]
struct EndpointSet {
    zen_inference: String,
    zen_catalog: String,
    go_inference: String,
    go_chat_inference: String,
    go_catalog: String,
}

impl EndpointSet {
    fn production() -> Self {
        Self {
            zen_inference: ZEN_INFERENCE_URI.to_owned(),
            zen_catalog: ZEN_CATALOG_URI.to_owned(),
            go_inference: GO_INFERENCE_URI.to_owned(),
            go_chat_inference: GO_CHAT_INFERENCE_URI.to_owned(),
            go_catalog: GO_CATALOG_URI.to_owned(),
        }
    }

    fn inference(&self, service: OpenCodeService, protocol: ProviderProtocol) -> Option<&str> {
        match (service, protocol) {
            (OpenCodeService::Zen, ProviderProtocol::Responses) => Some(&self.zen_inference),
            (OpenCodeService::Go, ProviderProtocol::Responses) => Some(&self.go_inference),
            (OpenCodeService::Go, ProviderProtocol::ChatCompletions) => {
                Some(&self.go_chat_inference)
            }
            (OpenCodeService::Zen, ProviderProtocol::ChatCompletions) => None,
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
    body: Vec<u8>,
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
            zen_catalog: format!("{base}/zen/v1/models"),
            go_inference: format!("{base}/zen/go/v1/responses"),
            go_chat_inference: format!("{base}/zen/go/v1/chat/completions"),
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
        let body = request.encode_body()?;
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
        }
    }

    fn push(&mut self, data: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        match self {
            Self::Responses(decoder) => decoder.push(data),
            Self::ChatCompletions(decoder) => decoder.push(data),
        }
    }

    fn finish(self) -> Result<ProviderOutcome, ProviderError> {
        match self {
            Self::Responses(decoder) => decoder.finish(),
            Self::ChatCompletions(decoder) => decoder.finish(),
        }
    }

    #[cfg(debug_assertions)]
    fn chat_diagnostic_stage(&self) -> Option<&'static str> {
        match self {
            Self::Responses(_) => None,
            Self::ChatCompletions(decoder) => Some(decoder.diagnostic_stage()),
        }
    }

    #[cfg(test)]
    fn responses_diagnostic(&self) -> Option<ResponsesDiagnostic> {
        match self {
            Self::Responses(decoder) => Some(decoder.diagnostic()),
            Self::ChatCompletions(_) => None,
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
        body: Vec<u8>,
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
        body: Vec<u8>,
        cancellation: &mut ProviderCancellation,
    ) -> Result<(Response<Incoming>, Instant), ProviderError> {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let authorization = authorization_header(api_key)?;
        let http_request = Request::builder()
            .method(Method::POST)
            .uri(parse_uri(
                self.endpoints
                    .inference(request.model().service, request.model().protocol)
                    .ok_or(ProviderError::UnsupportedModel)?,
            )?)
            .header(ACCEPT, "text/event-stream")
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(OPENCODE_SESSION_HEADER, request.opencode_session_header())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .body(Full::new(Bytes::from(body)))
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
        let body = request.encode_body()?;
        let (response, deadline) = self
            .send_inference(api_key, request, body, cancellation)
            .await?;
        self.consume_inference(response, deadline, request, cancellation, on_event)
            .await
    }

    fn build(endpoints: EndpointSet, allow_http: bool) -> Self {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        http.set_nodelay(true);
        let tls = HttpsConnectorBuilder::new().with_webpki_roots();
        let tls = if allow_http {
            tls.https_or_http()
        } else {
            tls.https_only()
        };
        let connector = tls.enable_http1().wrap_connector(http);
        let mut builder = Client::builder(TokioExecutor::new());
        builder.retry_canceled_requests(false);
        builder.pool_idle_timeout(Duration::from_secs(30));
        builder.pool_max_idle_per_host(2);
        Self {
            client: builder.build(connector),
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
    let mut header = HeaderValue::from_bytes(&value).map_err(|_| ProviderError::InvalidRequest)?;
    header.set_sensitive(true);
    Ok(header)
}

fn validate_response_headers(headers: &HeaderMap) -> Result<(), ProviderError> {
    if headers.len() > MAX_RESPONSE_HEADERS {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    let mut total = 0_usize;
    for (name, value) in headers {
        total = total
            .checked_add(name.as_str().len())
            .and_then(|bytes| bytes.checked_add(value.as_bytes().len()))
            .filter(|bytes| *bytes <= MAX_RESPONSE_HEADER_BYTES)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
    }
    Ok(())
}

fn validate_content_length(headers: &HeaderMap, maximum: usize) -> Result<(), ProviderError> {
    let Some(value) = headers.get(CONTENT_LENGTH) else {
        return Ok(());
    };
    let length = value
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(ProviderError::MalformedResponse)?;
    if length > maximum as u64 {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    Ok(())
}

fn require_content_type(headers: &HeaderMap, expected: &str) -> Result<(), ProviderError> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| value.eq_ignore_ascii_case(expected))
        .ok_or(ProviderError::UnexpectedContentType)?;
    let _ = content_type;
    Ok(())
}

fn classify_status(status: StatusCode) -> ProviderError {
    match status.as_u16() {
        300..=399 => ProviderError::RedirectDenied,
        401 | 403 => ProviderError::AuthenticationOrEntitlement,
        408 | 500..=599 => ProviderError::Unavailable,
        429 => ProviderError::RateLimited,
        _ => ProviderError::RequestRejected,
    }
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

async fn read_response_body_with_cancellation(
    mut body: Incoming,
    maximum_bytes: usize,
    deadline: Instant,
    cancellation: &mut ProviderCancellation,
) -> Result<(), ProviderError> {
    let mut bytes = 0_usize;
    while let Some(frame) = next_frame(&mut body, deadline, cancellation).await? {
        let data = frame
            .into_data()
            .map_err(|_| ProviderError::MalformedResponse)?;
        bytes = bytes
            .checked_add(data.len())
            .filter(|bytes| *bytes <= maximum_bytes)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
    }
    Ok(())
}

async fn next_frame(
    body: &mut Incoming,
    total_deadline: Instant,
    cancellation: &mut ProviderCancellation,
) -> Result<Option<Frame<Bytes>>, ProviderError> {
    if cancellation.is_cancelled() {
        return Err(ProviderError::Cancelled);
    }
    let now = Instant::now();
    if now >= total_deadline {
        return Err(ProviderError::TotalTimeout);
    }
    let inactivity_deadline = now + STREAM_INACTIVITY_TIMEOUT;
    let frame_deadline = inactivity_deadline.min(total_deadline);
    let result = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(ProviderError::Cancelled),
        result = time::timeout_at(frame_deadline, body.frame()) => result,
    };
    match result {
        Ok(Some(Ok(frame))) => Ok(Some(frame)),
        Ok(Some(Err(_))) => Err(ProviderError::Transport),
        Ok(None) => Ok(None),
        Err(_) if frame_deadline == total_deadline => Err(ProviderError::TotalTimeout),
        Err(_) => Err(ProviderError::StreamInactivityTimeout),
    }
}

#[cfg(test)]
mod tests;
