use super::{ProviderCancellation, ProviderError};
use bytes::Bytes;
use http::{
    HeaderMap, StatusCode,
    header::{CONTENT_LENGTH, CONTENT_TYPE},
};
use http_body_util::BodyExt as _;
use hyper::body::{Frame, Incoming};
use std::time::Duration;
use tokio::time::{self, Instant};

pub(super) const RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const STREAM_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const PROVIDER_TOTAL_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub(super) const MAX_RESPONSE_HEADERS: usize = 64;
pub(super) const MAX_RESPONSE_HEADER_BYTES: usize = 16 * 1024;
pub(super) const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

pub(super) fn validate_response_headers(headers: &HeaderMap) -> Result<(), ProviderError> {
    if headers.len() > MAX_RESPONSE_HEADERS {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    let mut total = 0_usize;
    for (name, value) in headers {
        total = total
            .checked_add(name.as_str().len())
            .and_then(|n| n.checked_add(value.as_bytes().len()))
            .filter(|n| *n <= MAX_RESPONSE_HEADER_BYTES)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
    }
    Ok(())
}
pub(super) fn validate_content_length(
    headers: &HeaderMap,
    maximum: usize,
) -> Result<(), ProviderError> {
    let Some(value) = headers.get(CONTENT_LENGTH) else {
        return Ok(());
    };
    let length = value
        .to_str()
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or(ProviderError::MalformedResponse)?;
    if length > maximum as u64 {
        return Err(ProviderError::ResponseLimitExceeded);
    }
    Ok(())
}
pub(super) fn require_content_type(
    headers: &HeaderMap,
    expected: &str,
) -> Result<(), ProviderError> {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        .filter(|v| v.eq_ignore_ascii_case(expected))
        .ok_or(ProviderError::UnexpectedContentType)?;
    Ok(())
}
pub(super) fn classify_status(status: StatusCode) -> ProviderError {
    match status.as_u16() {
        300..=399 => ProviderError::RedirectDenied,
        401 | 403 => ProviderError::AuthenticationOrEntitlement,
        408 | 500..=599 => ProviderError::Unavailable,
        429 => ProviderError::RateLimited,
        _ => ProviderError::RequestRejected,
    }
}
pub(super) async fn read_response_body_with_cancellation(
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
            .filter(|n| *n <= maximum_bytes)
            .ok_or(ProviderError::ResponseLimitExceeded)?;
    }
    Ok(())
}
pub(super) async fn next_frame(
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
    let frame_deadline = (now + STREAM_INACTIVITY_TIMEOUT).min(total_deadline);
    let result = tokio::select! {
        biased;
        ()=cancellation.cancelled()=>return Err(ProviderError::Cancelled),
        result=time::timeout_at(frame_deadline,body.frame())=>result,
    };
    match result {
        Ok(Some(Ok(frame))) => Ok(Some(frame)),
        Ok(Some(Err(_))) => Err(ProviderError::Transport),
        Ok(None) => Ok(None),
        Err(_) if frame_deadline == total_deadline => Err(ProviderError::TotalTimeout),
        Err(_) => Err(ProviderError::StreamInactivityTimeout),
    }
}
