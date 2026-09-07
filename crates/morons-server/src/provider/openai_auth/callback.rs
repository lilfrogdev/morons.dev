mod query;
use super::OAuthError;
#[cfg(test)]
pub(super) use query::decode_component;
use query::parse_query;
use std::{borrow::Cow, net::SocketAddr};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::{self, Duration},
};
use zeroize::Zeroizing;

const MAX_REQUEST: usize = 8192;
const MAX_CONNECTIONS: usize = 16;
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const RECEIVED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\nAuthorization code received. Return to Morons; login is not complete until credentials are saved.\n";
const DENIED: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\nAuthorization was denied. Return to Morons; no credential was installed by this callback.\n";
const REJECTED_HEADERS: &str = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n";

pub(super) struct Callback {
    listener: TcpListener,
    host: String,
}

enum Reply {
    Code(Zeroizing<String>),
    Denied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rejection {
    RequestFormat,
    RequestBounds,
    RequestTimeout,
    RequestIncomplete,
    Headers,
    Host,
    Path,
    QueryFormat,
    DuplicateField,
    State,
    Issuer,
    ResponseShape,
}
impl Rejection {
    fn label(self) -> &'static str {
        match self {
            Self::RequestFormat => "request-format",
            Self::RequestBounds => "request-bounds",
            Self::RequestTimeout => "request-timeout",
            Self::RequestIncomplete => "request-incomplete",
            Self::Headers => "headers",
            Self::Host => "host",
            Self::Path => "path",
            Self::QueryFormat => "query-format",
            Self::DuplicateField => "duplicate-field",
            Self::State => "state",
            Self::Issuer => "issuer",
            Self::ResponseShape => "response-shape",
        }
    }
}
fn response(reply: &Result<Reply, Rejection>) -> Cow<'static, [u8]> {
    match reply {
        Ok(Reply::Code(_)) => Cow::Borrowed(RECEIVED),
        Ok(Reply::Denied) => Cow::Borrowed(DENIED),
        Err(reason) => Cow::Owned(format!("{REJECTED_HEADERS}Callback rejected ({}). Return to Morons. Share only this reason, never the callback URL or code.\n", reason.label()).into_bytes()),
    }
}

impl Callback {
    pub(super) async fn bind(address: SocketAddr, host: String) -> Result<Self, OAuthError> {
        let listener = TcpListener::bind(address)
            .await
            .map_err(|_| OAuthError::CallbackUnavailable)?;
        Ok(Self { listener, host })
    }
    pub(super) async fn receive(&self, state: &str) -> Result<Zeroizing<String>, OAuthError> {
        for _ in 0..MAX_CONNECTIONS {
            let (mut stream, peer) = self
                .listener
                .accept()
                .await
                .map_err(|_| OAuthError::CallbackUnavailable)?;
            if !peer.ip().is_loopback() {
                continue;
            }
            if let Some(reply) = self.connection(&mut stream, state).await {
                return match reply {
                    Reply::Code(code) => Ok(code),
                    Reply::Denied => Err(OAuthError::AuthorizationDenied),
                };
            }
        }
        Err(OAuthError::CallbackLimit)
    }
    async fn connection(&self, stream: &mut TcpStream, state: &str) -> Option<Reply> {
        let deadline = time::Instant::now() + CONNECTION_TIMEOUT;
        let request = time::timeout_at(deadline, read_request(stream))
            .await
            .map_err(|_| Rejection::RequestTimeout)
            .and_then(|result| result);
        let reply = request
            .as_deref()
            .map_err(|reason| *reason)
            .and_then(|bytes| parse_request(bytes, &self.host, state));
        let response = response(&reply);
        let _ = time::timeout_at(deadline, stream.write_all(&response)).await;
        let _ = time::timeout_at(deadline, stream.shutdown()).await;
        reply.ok()
    }
    #[cfg(test)]
    pub(super) async fn for_test() -> Self {
        let mut callback = Self::bind("127.0.0.1:0".parse().unwrap(), String::new())
            .await
            .unwrap();
        callback.host = format!("localhost:{}", callback.address().port());
        callback
    }
    #[cfg(test)]
    pub(super) fn address(&self) -> SocketAddr {
        self.listener.local_addr().unwrap()
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<Zeroizing<Vec<u8>>, Rejection> {
    let mut output = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0_u8; 1024]);
    while output.len() < MAX_REQUEST {
        let limit = chunk.len().min(MAX_REQUEST - output.len());
        let count = stream
            .read(&mut chunk[..limit])
            .await
            .map_err(|_| Rejection::RequestIncomplete)?;
        if count == 0 {
            return Err(Rejection::RequestIncomplete);
        }
        output.extend_from_slice(&chunk[..count]);
        if let Some(end) = output.windows(4).position(|part| part == b"\r\n\r\n") {
            return if end + 4 == output.len() {
                Ok(output)
            } else {
                Err(Rejection::RequestFormat)
            };
        }
    }
    Err(Rejection::RequestBounds)
}

fn parse_request(bytes: &[u8], host: &str, state: &str) -> Result<Reply, Rejection> {
    if bytes.len() > MAX_REQUEST {
        return Err(Rejection::RequestBounds);
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Rejection::RequestFormat)?
        .strip_suffix("\r\n\r\n")
        .ok_or(Rejection::RequestFormat)?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next().ok_or(Rejection::RequestFormat)?.split(' ');
    if request.next() != Some("GET") {
        return Err(Rejection::RequestFormat);
    }
    let target = request.next().ok_or(Rejection::RequestFormat)?;
    if request.next() != Some("HTTP/1.1") || request.next().is_some() {
        return Err(Rejection::RequestFormat);
    }
    let query = target
        .strip_prefix("/auth/callback?")
        .ok_or(Rejection::Path)?;
    let mut names = Vec::new();
    let mut has_host = false;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or(Rejection::Headers)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            || names.len() >= 32
        {
            return Err(Rejection::Headers);
        }
        if !value
            .bytes()
            .all(|c| c == b'\t' || (0x20..=0x7e).contains(&c))
        {
            return Err(Rejection::Headers);
        }
        let name = name.to_ascii_lowercase();
        if names.contains(&name) {
            return Err(Rejection::Headers);
        }
        let value = value.trim_matches([' ', '\t']);
        match name.as_str() {
            "host" => {
                if value != host {
                    return Err(Rejection::Host);
                }
                has_host = true;
            }
            "content-length" if value == "0" => {}
            "content-length" | "transfer-encoding" | "expect" | "origin" => {
                return Err(Rejection::Headers);
            }
            _ => {}
        }
        names.push(name);
    }
    if !has_host {
        return Err(Rejection::Host);
    }
    parse_query(query, state)
}

#[cfg(test)]
mod tests;
