mod lifecycle;

use super::*;
use crate::provider::provider_cancellation;
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

async fn login(uri: http::Uri, slot: &Arc<Semaphore>, lifetime: Duration) -> OpenAiLogin {
    let callback = Callback::for_test().await;
    let redirect = format!(
        "http://localhost:{}/auth/callback",
        callback.address().port()
    );
    OpenAiLogin::create(
        callback,
        redirect,
        TokenClient::for_test(uri),
        slot.clone().try_acquire_owned().unwrap(),
        Instant::now() + lifetime,
    )
    .unwrap()
}
fn fields(query: &str) -> BTreeMap<String, String> {
    query
        .split('&')
        .map(|part| {
            let (k, v) = part.split_once('=').unwrap();
            (
                callback::decode_component(k).unwrap().to_string(),
                callback::decode_component(v).unwrap().to_string(),
            )
        })
        .collect()
}
async fn submit(address: std::net::SocketAddr, state: String, code: &str) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let query = form(&[("code", code), ("state", &state)]);
    stream
        .write_all(
            format!(
                "GET /auth/callback?{} HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
                query.as_str(),
                address.port()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    response
}
async fn mock_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let count = stream.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() < 65536);
        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = headers
                .lines()
                .filter_map(|s| s.split_once(':'))
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .unwrap()
                .1
                .trim()
                .parse::<usize>()
                .unwrap();
            if bytes.len() == end + 4 + length {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }
}

#[test]
fn pkce_encoding_matches_rfc7636_and_forms_do_not_inject_fields() {
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        )),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
    let first = random_value().unwrap();
    let second = random_value().unwrap();
    assert_eq!(first.len(), 43);
    assert_ne!(*first, *second);
    let form = form(&[("code", "a+&=% /"), ("scope", SCOPE)]);
    assert_eq!(fields(&form)["code"], "a+&=% /");
    assert_eq!(fields(&form).len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn complete_flow_is_one_exchange_fixed_fields_and_redacted() {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}/oauth/token", provider.local_addr().unwrap())
        .parse()
        .unwrap();
    let slot = Arc::new(Semaphore::new(1));
    let login = login(uri, &slot, Duration::from_secs(5)).await;
    let query = fields(
        login
            .authorization_url()
            .as_str()
            .split_once('?')
            .unwrap()
            .1,
    );
    assert!(
        login
            .authorization_url()
            .as_str()
            .starts_with(AUTHORIZE_URI)
    );
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(query["scope"], SCOPE);
    assert_eq!(query["originator"], "morons");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(
        query["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(login.verifier.as_bytes()))
    );
    assert_eq!(query["state"], login.state.as_str());
    assert_ne!(login.state.as_str(), login.verifier.as_str());
    assert!(!format!("{login:?} {:?}", login.authorization_url()).contains(login.state.as_str()));
    assert!(slot.clone().try_acquire_owned().is_err());
    let expected_verifier = login.verifier.to_string();
    let expected_redirect = login.redirect.clone();
    let (_, mut cancel) = provider_cancellation();
    let exchange = async {
        let (mut stream, _) = provider.accept().await.unwrap();
        let request = mock_request(&mut stream).await;
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /oauth/token HTTP/1.1"));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        let body = fields(body);
        assert_eq!(body.len(), 5);
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["client_id"], CLIENT_ID);
        assert_eq!(body["code"], "code+fixture");
        assert_eq!(body["code_verifier"], expected_verifier);
        assert_eq!(body["redirect_uri"], expected_redirect);
        let bytes = token::tests::response(now_seconds().unwrap());
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len()).as_bytes()).await.unwrap();
        stream.write_all(&bytes).await.unwrap();
    };
    let address = login.callback.address();
    let state = login.state.to_string();
    let send = submit(address, state.clone(), "code+fixture");
    let (response, result, ()) = tokio::join!(send, login.complete(&mut cancel), exchange);
    let tokens = result.unwrap();
    assert!(tokens.expires_at_seconds() > now_seconds().unwrap() + 300);
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(!response.contains("code+fixture"));
    assert!(!response.contains(&state));
    assert_eq!(slot.available_permits(), 1);
    assert!(TcpStream::connect(address).await.is_err());
    assert!(
        time::timeout(Duration::from_millis(50), provider.accept())
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_deadline_and_drop_release_listener_and_admission() {
    let slot = Arc::new(Semaphore::new(1));
    let uri = "http://127.0.0.1:1/oauth/token"
        .parse::<http::Uri>()
        .unwrap();
    for kind in 0..3 {
        let login = login(uri.clone(), &slot, Duration::from_millis(50)).await;
        let address = login.callback.address();
        let (handle, mut cancel) = provider_cancellation();
        if kind == 0 {
            drop(login);
        } else {
            if kind == 1 {
                handle.cancel();
            }
            let error = login.complete(&mut cancel).await.unwrap_err();
            assert_eq!(
                error,
                if kind == 1 {
                    OAuthError::Cancelled
                } else {
                    OAuthError::Deadline
                }
            );
        }
        assert!(TcpStream::connect(address).await.is_err());
        assert_eq!(slot.available_permits(), 1);
    }
    let login = login(uri, &slot, Duration::from_millis(50)).await;
    let address = login.callback.address();
    let mut stalled = TcpStream::connect(address).await.unwrap();
    stalled
        .write_all(b"GET /auth/callback?code=stalled")
        .await
        .unwrap();
    let (_, mut cancel) = provider_cancellation();
    assert_eq!(
        login.complete(&mut cancel).await.unwrap_err(),
        OAuthError::Deadline
    );
    let mut b = [0];
    assert_eq!(stalled.read(&mut b).await.unwrap(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn exchange_rejections_truncation_and_redirects_never_retry_or_follow() {
    for response in [
        "HTTP/1.1 302 Found\r\nLocation: http://TOKEN_LISTENER/stolen\r\nContent-Length: 0\r\n\r\n"
            .to_owned(),
        "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n".to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 0\r\n\r\n".to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 65537\r\n\r\n"
            .to_owned(),
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{}"
            .to_owned(),
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Oversize: {}\r\nContent-Length: 0\r\n\r\n",
            "x".repeat(9000)
        ),
    ] {
        let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let slot = Arc::new(Semaphore::new(1));
        let uri = format!("http://{}/oauth/token", provider.local_addr().unwrap())
            .parse()
            .unwrap();
        let login = login(uri, &slot, Duration::from_secs(3)).await;
        let send = submit(
            login.callback.address(),
            login.state.to_string(),
            "code-fixture",
        );
        let exchange = async {
            let (mut stream, _) = provider.accept().await.unwrap();
            let _ = mock_request(&mut stream).await;
            let response = response.replace(
                "TOKEN_LISTENER",
                &provider.local_addr().unwrap().to_string(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        };
        let (_, mut cancel) = provider_cancellation();
        let (_, result, ()) = tokio::join!(send, login.complete(&mut cancel), exchange);
        assert!(result.is_err());
        assert_eq!(slot.available_permits(), 1);
        assert!(
            time::timeout(Duration::from_millis(30), provider.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_after_exchange_dispatch_closes_without_returning_tokens() {
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let slot = Arc::new(Semaphore::new(1));
    let uri = format!("http://{}/oauth/token", provider.local_addr().unwrap())
        .parse()
        .unwrap();
    let login = login(uri, &slot, Duration::from_secs(3)).await;
    let send = submit(
        login.callback.address(),
        login.state.to_string(),
        "code-fixture",
    );
    let (handle, mut cancel) = provider_cancellation();
    let (tx, rx) = oneshot::channel();
    let exchange = async {
        let (mut stream, _) = provider.accept().await.unwrap();
        let _ = mock_request(&mut stream).await;
        tx.send(()).unwrap();
        let mut bytes = [0; 1];
        assert_eq!(stream.read(&mut bytes).await.unwrap(), 0);
    };
    let cancel_when_sent = async {
        rx.await.unwrap();
        handle.cancel();
    };
    let (_, result, (), ()) = tokio::join!(
        send,
        login.complete(&mut cancel),
        exchange,
        cancel_when_sent
    );
    assert_eq!(result.unwrap_err(), OAuthError::Cancelled);
    assert_eq!(slot.available_permits(), 1);
}
