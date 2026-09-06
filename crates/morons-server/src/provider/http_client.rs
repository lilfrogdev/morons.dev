use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};

pub(crate) type ProviderHttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

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
