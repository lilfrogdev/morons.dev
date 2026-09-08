use super::*;
use crate::provider::openai_auth::TokenResponseFailure as Source;
use morons_protocol::OpenAiTokenResponseFailure as Target;

#[test]
fn token_failure_conversion_preserves_only_the_closed_reason() {
    for (source, target) in [
        (Source::Headers, Target::Headers),
        (Source::BodyBounds, Target::BodyBounds),
        (Source::BodyFraming, Target::BodyFraming),
        (Source::Json, Target::Json),
        (Source::TokenFields, Target::TokenFields),
        (Source::TokenType, Target::TokenType),
        (Source::Scope, Target::Scope),
        (Source::ResponseLifetime, Target::ResponseLifetime),
        (Source::AccessTokenFormat, Target::AccessTokenFormat),
        (Source::ClaimsJson, Target::ClaimsJson),
        (Source::ClaimExpiry, Target::ClaimExpiry),
        (Source::AccountClaim, Target::AccountClaim),
        (Source::EffectiveLifetime, Target::EffectiveLifetime),
        (Source::Clock, Target::Clock),
        (Source::StoredCredential, Target::StoredCredential),
    ] {
        let error = OAuthError::InvalidTokenResponse(source);
        assert_eq!(
            oauth_failure(error),
            Failure::InvalidResponse { reason: target }
        );
        assert!(error.to_string().contains(&format!("({source})")));
        assert!(std::error::Error::source(&error).is_none());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_token_exchange_sends_only_fixed_reason_and_preserves_installed_bytes() {
    let (root, store, supervisor) = setup();
    store
        .set_openai_credential(
            crate::persistence::MutationRequestId::from_bytes([1; 16]),
            0,
            tokens(),
        )
        .await
        .unwrap();
    let path = root.path().join("credentials/openai-chatgpt.state");
    let before = fs::read(&path).unwrap(); // synthetic private test root only
    let provider = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let connection = start(&supervisor, &provider, 1).await;
    let (mut client, mut server) = tokio::io::duplex(8192);
    let task = tokio::spawn(async move {
        crate::connection::stream_openai_login(&mut server, 7, connection)
            .await
            .unwrap();
    });
    let Some(ServerMessage::Response {
        response: ApplicationResponse::OpenAiLoginStarted { url, .. },
        ..
    }) = read_server_message(&mut client).await.unwrap()
    else {
        panic!("start expected")
    };
    let (address, state) = callback(&url);
    drop(url);
    let reply = async {
        let (mut stream, _) = provider.accept().await.unwrap();
        let request = mock_request(&mut stream).await;
        assert_eq!(fields(request.split_once("\r\n\r\n").unwrap().1).len(), 5);
        let tokens = tokens();
        let (access, refresh, _, _) = tokens.stored_parts();
        let body = serde_json::to_vec(&serde_json::json!({
            "access_token":access, "refresh_token":refresh, "expires_in":3600,
            "scope":"PRIVATE-unreviewed-scope", "PRIVATE-metadata":"PRIVATE-value",
        }))
        .unwrap();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
    };
    let (response, ()) = tokio::join!(submit(address, state, "PRIVATE-code"), reply);
    assert!(response.starts_with(b"HTTP/1.1 200"));
    let message = read_server_message(&mut client).await.unwrap().unwrap();
    assert_eq!(
        message,
        ServerMessage::OpenAiLoginFinished {
            attempt_id: id(),
            outcome: Outcome::Failed {
                failure: Failure::InvalidResponse {
                    reason: Target::Scope
                },
            },
        }
    );
    let encoded = String::from_utf8(message.encode_json().unwrap()).unwrap();
    assert!(!encoded.contains("PRIVATE"));
    assert!(!encoded.contains("fixture"));
    task.await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        1
    );
    assert!(supervisor.active.lock().await.is_none());
    assert!(TcpStream::connect(address).await.is_err());
    assert!(
        time::timeout(Duration::from_millis(30), provider.accept())
            .await
            .is_err()
    );
    supervisor.shutdown().await;
}
