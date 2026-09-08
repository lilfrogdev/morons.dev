use super::{
    OAuthError, OAuthTokens, TOKEN_URI, TokenResponseFailure as Reason, now_seconds,
    token::parse_tokens,
};
use crate::provider::http_client::{ProviderHttpClient, bounded_client};
use bytes::Bytes;
use http::{
    Request, Uri,
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::{BodyExt as _, Full};
use std::time::Duration;
use tokio::time;
use zeroize::Zeroizing;

const MAX_HEADERS: usize = 32;
const MAX_HEADER_BYTES: usize = 8192;
const MAX_BODY: usize = 64 * 1024;
const IDLE: Duration = Duration::from_secs(10);
const TOTAL: Duration = Duration::from_secs(30);

pub(super) struct TokenClient {
    client: ProviderHttpClient,
    uri: Uri,
}
impl TokenClient {
    pub(super) fn new() -> Self {
        Self {
            client: bounded_client(false, Some((MAX_HEADERS, MAX_HEADER_BYTES))),
            uri: TOKEN_URI.parse().expect("fixed token URI"),
        }
    }
    #[cfg(test)]
    pub(super) fn for_test(uri: Uri) -> Self {
        Self {
            client: bounded_client(true, Some((MAX_HEADERS, MAX_HEADER_BYTES))),
            uri,
        }
    }

    pub(super) async fn exchange(
        &self,
        form: Zeroizing<String>,
    ) -> Result<OAuthTokens, OAuthError> {
        time::timeout(TOTAL, self.exchange_inner(form))
            .await
            .map_err(|_| OAuthError::ExchangeUncertain)?
    }
    async fn exchange_inner(&self, form: Zeroizing<String>) -> Result<OAuthTokens, OAuthError> {
        let request = Request::builder()
            .method("POST")
            .uri(self.uri.clone())
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(
                USER_AGENT,
                concat!("morons-server/", env!("CARGO_PKG_VERSION")),
            )
            .body(Full::new(Bytes::from_owner(form)))
            .map_err(|_| OAuthError::ExchangeUncertain)?;
        let response = time::timeout(IDLE, self.client.request(request))
            .await
            .map_err(|_| OAuthError::ExchangeUncertain)?
            .map_err(|_| OAuthError::ExchangeUncertain)?;
        let headers = response.headers();
        if headers.len() > MAX_HEADERS
            || headers
                .iter()
                .map(|(k, v)| k.as_str().len() + v.as_bytes().len())
                .sum::<usize>()
                > MAX_HEADER_BYTES
        {
            return Err(OAuthError::InvalidTokenResponse(Reason::Headers));
        }
        if response.status().as_u16() != 200 {
            return Err(match response.status().as_u16() {
                400 | 401 | 403 => OAuthError::TokenRejected,
                _ => OAuthError::ExchangeUncertain,
            });
        }
        if headers.get_all(CONTENT_TYPE).iter().count() != 1
            || headers
                .get(CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.split(';').next())
                .is_none_or(|s| !s.trim().eq_ignore_ascii_case("application/json"))
            || headers.get_all(CONTENT_LENGTH).iter().count() > 1
        {
            return Err(OAuthError::InvalidTokenResponse(Reason::Headers));
        }
        if let Some(length) = headers.get(CONTENT_LENGTH)
            && length
                .to_str()
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .is_none_or(|n| n > MAX_BODY as u64)
        {
            return Err(OAuthError::InvalidTokenResponse(Reason::BodyBounds));
        }
        let mut body = response.into_body();
        let mut bytes = Zeroizing::new(Vec::new());
        while let Some(frame) = time::timeout(IDLE, body.frame())
            .await
            .map_err(|_| OAuthError::ExchangeUncertain)?
        {
            let data = frame
                .map_err(|_| OAuthError::ExchangeUncertain)?
                .into_data()
                .map_err(|_| OAuthError::InvalidTokenResponse(Reason::BodyFraming))?;
            if data.len() > MAX_BODY.saturating_sub(bytes.len()) {
                return Err(OAuthError::InvalidTokenResponse(Reason::BodyBounds));
            }
            bytes.extend_from_slice(&data);
        }
        parse_tokens(&bytes, now_seconds()?)
    }
}
