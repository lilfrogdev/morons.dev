use super::OAuthError;
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use std::net::SocketAddr;
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
const REJECTED: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\nCallback rejected. Return to Morons.\n";

pub(super) struct Callback {
    listener: TcpListener,
    host: String,
}

enum Reply {
    Code(Zeroizing<String>),
    Denied,
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
            .ok()
            .flatten();
        let reply = request
            .as_deref()
            .and_then(|bytes| parse_request(bytes, &self.host, state));
        let response = if matches!(reply, Some(Reply::Code(_))) {
            RECEIVED
        } else {
            REJECTED
        };
        let _ = time::timeout_at(deadline, stream.write_all(response)).await;
        let _ = time::timeout_at(deadline, stream.shutdown()).await;
        reply
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

async fn read_request(stream: &mut TcpStream) -> Option<Zeroizing<Vec<u8>>> {
    let mut output = Zeroizing::new(Vec::new());
    let mut chunk = Zeroizing::new([0_u8; 1024]);
    while output.len() < MAX_REQUEST {
        let limit = chunk.len().min(MAX_REQUEST - output.len());
        let count = stream.read(&mut chunk[..limit]).await.ok()?;
        if count == 0 {
            return None;
        }
        output.extend_from_slice(&chunk[..count]);
        if let Some(end) = output.windows(4).position(|part| part == b"\r\n\r\n") {
            return (end + 4 == output.len()).then_some(output);
        }
    }
    None
}

fn parse_request(bytes: &[u8], host: &str, state: &str) -> Option<Reply> {
    if bytes.len() > MAX_REQUEST {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?.strip_suffix("\r\n\r\n")?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next()?.split(' ');
    if request.next()? != "GET" {
        return None;
    }
    let target = request.next()?;
    if request.next()? != "HTTP/1.1" || request.next().is_some() {
        return None;
    }
    let query = target.strip_prefix("/auth/callback?")?;
    let mut names = Vec::new();
    let mut has_host = false;
    for line in lines {
        let (name, value) = line.split_once(':')?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
            || names.len() >= 32
        {
            return None;
        }
        if !value
            .bytes()
            .all(|c| c == b'\t' || (0x20..=0x7e).contains(&c))
        {
            return None;
        }
        let name = name.to_ascii_lowercase();
        if names.contains(&name) {
            return None;
        }
        let value = value.trim_matches([' ', '\t']);
        match name.as_str() {
            "host" => {
                if value != host {
                    return None;
                }
                has_host = true;
            }
            "content-length" if value == "0" => {}
            "content-length" | "transfer-encoding" | "expect" | "origin" => return None,
            _ => {}
        }
        names.push(name);
    }
    if !has_host {
        return None;
    }
    parse_query(query, state)
}

fn parse_query(query: &str, expected_state: &str) -> Option<Reply> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (index, field) in query.split('&').enumerate() {
        if index >= 4 {
            return None;
        }
        let (key, value) = field.split_once('=')?;
        let key = decode_component(key)?;
        let value = decode_component(value)?;
        let slot = match key.as_str() {
            "code" => &mut code,
            "state" => &mut state,
            "error" => &mut error,
            "error_description" => &mut description,
            _ => return None,
        };
        if slot.replace(value).is_some() {
            return None;
        }
    }
    let state = state?;
    if state.len() != expected_state.len() || !state_matches(&state, expected_state) {
        return None;
    }
    match (code, error, description) {
        (Some(code), None, None)
            if !code.is_empty()
                && code.len() <= 4096
                && code.bytes().all(|c| (0x21..=0x7e).contains(&c)) =>
        {
            Some(Reply::Code(code))
        }
        (None, Some(error), description)
            if !error.is_empty()
                && error.len() <= 128
                && description.as_ref().is_none_or(|s| s.len() <= 2048) =>
        {
            Some(Reply::Denied)
        }
        _ => None,
    }
}

fn state_matches(actual: &str, expected: &str) -> bool {
    let base = Hmac::<Sha256>::new_from_slice(b"morons.dev/oauth-state/v1")
        .expect("HMAC accepts this key length");
    let mut wanted = base.clone();
    wanted.update(expected.as_bytes());
    let mut supplied = base;
    supplied.update(actual.as_bytes());
    wanted
        .verify_slice(&supplied.finalize().into_bytes())
        .is_ok()
}

pub(super) fn decode_component(value: &str) -> Option<Zeroizing<String>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(value.len()));
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        let byte = match byte {
            b'%' => {
                let high = char::from(input.next()?).to_digit(16)?;
                let low = char::from(input.next()?).to_digit(16)?;
                u8::try_from(high * 16 + low).ok()?
            }
            b'+' => b' ',
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => byte,
            _ => return None,
        };
        if !(0x20..=0x7e).contains(&byte) {
            return None;
        }
        bytes.push(byte);
    }
    Some(Zeroizing::new(std::str::from_utf8(&bytes).ok()?.to_owned()))
}

#[cfg(test)]
mod tests;
