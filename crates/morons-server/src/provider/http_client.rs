use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

use super::{ProviderCancellation, ProviderError};
use http::{Request, Response};
use hyper::body::Incoming;
use tokio::time::{self, Instant};

pub(crate) type ProviderHttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

pub(crate) async fn send_model_request(
    client: &ProviderHttpClient,
    request: Request<Full<Bytes>>,
    header_timeout: Duration,
    deadline: Instant,
    cancellation: &mut ProviderCancellation,
) -> Result<Response<Incoming>, ProviderError> {
    for attempt in 0..=3 {
        if cancellation.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ProviderError::TotalTimeout);
        }
        let header_deadline = (Instant::now() + header_timeout).min(deadline);
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            result = time::timeout_at(header_deadline, client.request(request.clone())) => {
                result.map_err(|_| if header_deadline == deadline {
                    ProviderError::TotalTimeout
                } else {
                    ProviderError::ResponseHeaderTimeout
                })?
            }
        };
        match result {
            Ok(response) => return Ok(response),
            // Hyper's Connect failures precede request transmission; other failures are uncertain.
            Err(error) if error.is_connect() && attempt < 3 => {}
            Err(_) => return Err(ProviderError::Transport),
        }
        let wake = (Instant::now() + Duration::from_millis(250 << attempt)).min(deadline);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ProviderError::Cancelled),
            () = time::sleep_until(wake) => {}
        }
    }
    unreachable!("last attempt returns without backoff")
}

#[cfg(test)]
mod tests;

pub(crate) fn bounded_client(
    allow_http: bool,
    header_limits: Option<(usize, usize)>,
) -> ProviderHttpClient {
    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_connect_timeout(Some(Duration::from_secs(10)));
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
    if let Some((maximum_headers, maximum_header_bytes)) = header_limits {
        builder.http1_max_headers(maximum_headers);
        builder.http1_max_buf_size(maximum_header_bytes);
    }
    builder.pool_idle_timeout(Duration::from_secs(30));
    builder.pool_max_idle_per_host(2);
    builder.build(connector)
}
