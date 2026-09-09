use super::{MAX_RESPONSE_BYTES, SearchRequest, SearchResult, check_policy, decode_response};
use crate::provider::{
    DataUseRestrictions, ProviderCancellation, ProviderError,
    http_client::{ProviderHttpClient, bounded_client},
    openai_auth::{OpenAiCredentialLease, OpenAiCredentialProvider},
    openai_codex::{NATIVE_RESPONSES_ENDPOINT, credential_error},
    response_http::*,
};
use http::{
    Request, Uri,
    header::{ACCEPT, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::Full;
use sha2::{Digest as _, Sha256};
use std::{fmt, sync::Arc};
use tokio::time::{self, Instant};

pub struct SearchProvider {
    credentials: Arc<OpenAiCredentialProvider>,
    client: ProviderHttpClient,
    endpoint: Uri,
    registration: Arc<()>,
    total_timeout: std::time::Duration,
}
pub struct SearchAttempt {
    registration: Arc<()>,
    generation: u64,
    identifier: String,
    request_id: String,
    request: SearchRequest,
    spent: bool,
}
impl fmt::Debug for SearchAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchAttempt")
            .field("spent", &self.spent)
            .finish_non_exhaustive()
    }
}
pub struct PreparedSearch<'a> {
    provider: &'a SearchProvider,
    attempt: &'a mut SearchAttempt,
    lease: OpenAiCredentialLease<'a>,
}
impl SearchProvider {
    /// Trusted server construction only; this library does not authorize application dispatch.
    pub fn new(credentials: Arc<OpenAiCredentialProvider>) -> Self {
        Self {
            credentials,
            client: bounded_client(
                false,
                Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
            ),
            endpoint: NATIVE_RESPONSES_ENDPOINT
                .parse()
                .expect("reviewed native route"),
            registration: Arc::new(()),
            total_timeout: std::time::Duration::from_secs(120),
        }
    }
    pub fn new_attempt(
        &self,
        operation: [u8; 16],
        generation: u64,
        query: &str,
        policy: DataUseRestrictions,
    ) -> Result<SearchAttempt, ProviderError> {
        if operation == [0; 16] || generation == 0 || generation > i64::MAX as u64 {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(SearchAttempt {
            registration: self.registration.clone(),
            generation,
            identifier: identifier(b"morons.dev/openai-web-session/v1\0", operation, generation),
            request_id: identifier(b"morons.dev/openai-web-request/v1\0", operation, generation),
            request: SearchRequest::new(query, policy)?,
            spent: false,
        })
    }
    fn validate(
        &self,
        attempt: &SearchAttempt,
        policy: DataUseRestrictions,
    ) -> Result<(), ProviderError> {
        check_policy(policy)?;
        if attempt.spent || !Arc::ptr_eq(&attempt.registration, &self.registration) {
            return Err(ProviderError::InvalidRequest);
        }
        Ok(())
    }
    pub async fn prepare<'a>(
        &'a self,
        attempt: &'a mut SearchAttempt,
        policy: DataUseRestrictions,
        cancellation: &mut ProviderCancellation,
    ) -> Result<PreparedSearch<'a>, ProviderError> {
        self.validate(attempt, policy)?;
        let lease = self
            .credentials
            .lease(attempt.generation, cancellation)
            .await
            .map_err(credential_error)?;
        self.validate(attempt, policy)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        Ok(PreparedSearch {
            provider: self,
            attempt,
            lease,
        })
    }
    #[cfg(test)]
    pub(crate) fn for_test(credentials: Arc<OpenAiCredentialProvider>, endpoint: Uri) -> Self {
        let mut provider = Self::new(credentials);
        provider.client = bounded_client(
            true,
            Some((MAX_RESPONSE_HEADERS, MAX_RESPONSE_HEADER_BYTES)),
        );
        provider.endpoint = endpoint;
        provider
    }
}
impl PreparedSearch<'_> {
    /// Requires the caller's committed binding and serialized current-policy admission first.
    pub async fn execute(
        self,
        policy: DataUseRestrictions,
        cancellation: &mut ProviderCancellation,
    ) -> Result<SearchResult, ProviderError> {
        self.provider.validate(self.attempt, policy)?;
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        self.attempt.spent = true;
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
            .header("session-id", &self.attempt.identifier)
            .header("thread-id", &self.attempt.identifier)
            .header("x-client-request-id", &self.attempt.request_id)
            .body(Full::new(self.attempt.request.body().clone()))
            .map_err(|_| ProviderError::InvalidRequest)?;
        request
            .headers_mut()
            .extend(self.lease.authorization_headers());
        let start = Instant::now();
        let deadline = start + self.provider.total_timeout;
        let headers = (start + RESPONSE_HEADER_TIMEOUT).min(deadline);
        let response = tokio::select! {
            biased;
            ()=cancellation.cancelled()=>return Err(ProviderError::Cancelled),
            result=time::timeout_at(headers,self.provider.client.request(request))=>result.map_err(|_|if headers==deadline {ProviderError::TotalTimeout}else{ProviderError::ResponseHeaderTimeout})?.map_err(|_|ProviderError::Transport)?,
        };
        drop(self.lease);
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
        for name in [CONTENT_TYPE, http::header::CONTENT_LENGTH] {
            if response.headers().get_all(name).iter().nth(1).is_some() {
                return Err(ProviderError::MalformedResponse);
            }
        }
        if response.headers().contains_key(CONTENT_TYPE) {
            require_content_type(response.headers(), "text/event-stream")?;
        }
        validate_content_length(response.headers(), MAX_RESPONSE_BYTES)?;
        let mut incoming = response.into_body();
        let mut body = Vec::new();
        while let Some(frame) = next_frame(&mut incoming, deadline, cancellation).await? {
            let data = frame
                .into_data()
                .map_err(|_| ProviderError::MalformedResponse)?;
            if body
                .len()
                .checked_add(data.len())
                .is_none_or(|n| n > MAX_RESPONSE_BYTES)
            {
                return Err(ProviderError::ResponseLimitExceeded);
            }
            body.extend_from_slice(&data);
        }
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        decode_response(&body)
    }
}
fn identifier(domain: &[u8], operation: [u8; 16], generation: u64) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(operation);
    hash.update(generation.to_be_bytes());
    let hash = hash.finalize();
    let hex = hash[..16]
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

#[cfg(test)]
mod tests;
