mod denial;
use super::*;

fn request(query: &str) -> String {
    format!(
        "GET /auth/callback?{query} HTTP/1.1\r\nHost: localhost:1455\r\nUser-Agent: browser\r\n\r\n"
    )
}
#[test]
fn callback_grammar_rejects_ambiguous_hosts_headers_paths_and_queries() {
    let good = request("code=secret-code&state=expected");
    assert!(matches!(
        parse_request(good.as_bytes(), "localhost:1455", "expected"),
        Ok(Reply::Code(_))
    ));
    for query in [
        "code=secret-code&state=wrong",
        "code=secret-code",
        "state=expected",
        "code=&state=expected",
        "code=a&state=expected&code=b",
        "code=a&state=expected&co%64e=b",
        "code=a&state=expected&state=expected",
        "code=a&state=expected&error=denied",
        "code=%GG&state=expected",
        "code=%00&state=expected",
        "code=%0A&state=expected",
        "code=%&state=expected",
        "code=a&state=expected#fragment",
        "code=a&state=expected&error_description=no",
        "code=a&state=expected&",
        "error=denied&state=wrong",
    ] {
        assert!(
            parse_request(request(query).as_bytes(), "localhost:1455", "expected").is_err(),
            "query classification"
        );
    }
    for invalid in [
        good.replace("GET ", "POST "),
        good.replace("HTTP/1.1", "HTTP/1.0"),
        good.replace("/auth/callback?", "http://localhost:1455/auth/callback?"),
        good.replace("/auth/callback?", "/auth/callback/../callback?"),
        good.replace("localhost:1455", "evil.invalid:1455"),
        good.replace("Host: localhost:1455\r\n", ""),
        good.replace("User-Agent: browser", "host: localhost:1455"),
        good.replace("User-Agent: browser", "User-Agent: a\r\nuser-agent: b"),
        good.replace("User-Agent: browser", "Content-Length: 1"),
        good.replace("User-Agent: browser", "Transfer-Encoding: chunked"),
        good.replace("User-Agent: browser", "Origin: https://evil.invalid"),
        good.replace("User-Agent: browser", "Expect: 100-continue"),
        good.replace("User-Agent: browser", " Host: localhost:1455"),
        good.replace("User-Agent: browser", "User-Agent: bad\0value"),
        good.replace("User-Agent: browser", &"X: a\r\n".repeat(33)),
        good.replace(
            "User-Agent: browser",
            &format!("X: {}", "x".repeat(MAX_REQUEST)),
        ),
        format!("{good}smuggled-body"),
    ] {
        assert!(
            parse_request(invalid.as_bytes(), "localhost:1455", "expected").is_err(),
            "HTTP classification"
        );
    }
    assert!(matches!(
        parse_request(
            request("error=access_denied&error_description=not+approved&state=expected").as_bytes(),
            "localhost:1455",
            "expected"
        ),
        Ok(Reply::Denied)
    ));
    assert!(
        matches!(parse_request(request("code=a%2Bb%3Dc&state=expected").as_bytes(),"localhost:1455","expected"),Ok(Reply::Code(code)) if code.as_str()=="a+b=c")
    );
    assert!(
        parse_request(
            request(&format!("code={}&state=expected", "x".repeat(4097))).as_bytes(),
            "localhost:1455",
            "expected"
        )
        .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn callback_bounds_connections_and_does_not_take_over_an_occupied_port() {
    let callback = Callback::for_test().await;
    let address = callback.address();
    assert!(
        Callback::bind(address, callback.host.clone())
            .await
            .is_err()
    );
    let task = tokio::spawn(async move { callback.receive("state").await });
    for _ in 0..MAX_CONNECTIONS {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /wrong HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, super::response(&Err(Rejection::Path)).as_ref());
    }
    assert_eq!(task.await.unwrap().unwrap_err(), OAuthError::CallbackLimit);
    assert!(TcpStream::connect(address).await.is_err());
}

#[test]
fn browser_callback_extensions_and_query_literals_are_compatible() {
    for query in [
        "code=opaque-fixture&state=expected&scope=openid+profile+email+offline_access",
        "code=opaque-fixture&state=expected&session_state=metadata&scope=openid",
        "code=opaque-fixture&state=expected&iss=https%3A%2F%2Fauth.openai.com",
        "code=opaque-fixture&state=expected&iss=https://auth.openai.com&extension=a/b?c=d",
        "code=opaque!$'()*,;:@/=?&state=expected",
    ] {
        assert!(
            matches!(
                parse_request(request(query).as_bytes(), "localhost:1455", "expected"),
                Ok(Reply::Code(_))
            ),
            "bounded browser callback should be compatible"
        );
    }
}

#[test]
fn callback_extensions_are_bounded_duplicate_checked_and_cannot_change_authority() {
    for (query, reason) in [
        ("code=a&state=wrong&scope=openid", Rejection::State),
        ("code=a&scope=openid", Rejection::State),
        (
            "code=a&state=expected&scope=a&sco%70e=b",
            Rejection::DuplicateField,
        ),
        (
            "code=a&state=expected&unknown=a&unknown=b",
            Rejection::DuplicateField,
        ),
        (
            "code=a&state=expected&iss=https://evil.invalid",
            Rejection::Issuer,
        ),
        (
            "code=a&state=expected&iss=https://auth.openai.com/",
            Rejection::Issuer,
        ),
        (
            "code=a&state=expected&iss=https://auth.openai.com:443",
            Rejection::Issuer,
        ),
        ("code=a&state=expected&iss=", Rejection::Issuer),
        (
            "error=denied&state=expected&iss=https://evil.invalid",
            Rejection::Issuer,
        ),
        (
            "code=a&state=expected&iss=https://auth.openai.com&%69ss=https://auth.openai.com",
            Rejection::DuplicateField,
        ),
        (
            "code=a&state=expected&access_token=DO-NOT-IMPORT",
            Rejection::ResponseShape,
        ),
        (
            "code=a&state=expected&id_token=DO-NOT-IMPORT",
            Rejection::ResponseShape,
        ),
        (
            "code=a&state=expected&refresh_token=DO-NOT-IMPORT",
            Rejection::ResponseShape,
        ),
        (
            "code=a&state=expected&error=denied&scope=openid",
            Rejection::ResponseShape,
        ),
        ("code=a&state=expected&scope=%ZZ", Rejection::QueryFormat),
        ("code=a&state=expected&scope=%0a", Rejection::QueryFormat),
        ("code=a&state=expected&scope=%C3%A9", Rejection::QueryFormat),
        ("code=a&state=expected&=metadata", Rejection::QueryFormat),
    ] {
        let reply = parse_request(request(query).as_bytes(), "localhost:1455", "expected");
        assert_eq!(reply.err(), Some(reason));
    }
    for query in [
        format!("code=a&state=expected&{}=x", "n".repeat(129)),
        format!("code=a&state=expected&metadata={}", "x".repeat(4097)),
        format!(
            "code=a&state=expected{}",
            (0..15).map(|i| format!("&n{i}=x")).collect::<String>()
        ),
    ] {
        assert_eq!(
            parse_request(request(&query).as_bytes(), "localhost:1455", "expected").err(),
            Some(Rejection::QueryFormat)
        );
    }
    for query in [
        format!("code=a&state=expected&{}=x", "n".repeat(128)),
        format!("code=a&state=expected&metadata={}", "x".repeat(4096)),
        format!("code=a&state=expected{}", (0..14).map(|i| format!("&n{i}=x")).collect::<String>()),
        "code=a&state=expected&scope=untrusted-admin-claim&token_endpoint=https://evil.invalid&model=unsupported&training=not-used".into(),
    ] {
        assert!(matches!(parse_request(request(&query).as_bytes(), "localhost:1455", "expected"), Ok(Reply::Code(code)) if code.as_str() == "a"));
    }
    let base = request("code=a&state=expected").replace("User-Agent: browser", "X-Bound: ");
    let bounded = base.replace(
        "X-Bound: ",
        &format!("X-Bound: {}", "x".repeat(MAX_REQUEST - base.len())),
    );
    assert_eq!(bounded.len(), MAX_REQUEST);
    assert!(parse_request(bounded.as_bytes(), "localhost:1455", "expected").is_ok());
    assert_eq!(
        parse_request(
            format!("{bounded}x").as_bytes(),
            "localhost:1455",
            "expected"
        )
        .err(),
        Some(Rejection::RequestBounds)
    );
}

#[test]
fn callback_browser_diagnostics_are_fixed_and_never_reflect_request_data() {
    for reason in [
        Rejection::RequestFormat,
        Rejection::RequestBounds,
        Rejection::RequestTimeout,
        Rejection::RequestIncomplete,
        Rejection::Headers,
        Rejection::Host,
        Rejection::Path,
        Rejection::QueryFormat,
        Rejection::DuplicateField,
        Rejection::State,
        Rejection::Issuer,
        Rejection::ResponseShape,
    ] {
        let reply = Err(reason);
        let body = response(&reply);
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.starts_with("HTTP/1.1 400"));
        assert!(text.contains(reason.label()));
        assert!(text.contains("Cache-Control: no-store"));
        assert!(text.contains("default-src 'none'"));
        assert!(text.len() < 512);
    }
    for query in [
        "code=PRIVATE-CODE&state=PRIVATE-STATE",
        "code=PRIVATE-CODE&state=expected&PRIVATE-FIELD=%00",
        "error=PRIVATE-ERROR&error_description=PRIVATE-DESCRIPTION&state=expected&iss=https://auth.openai.com",
    ] {
        let reply = parse_request(request(query).as_bytes(), "localhost:1455", "expected");
        let body = response(&reply);
        let text = std::str::from_utf8(&body).unwrap();
        assert!(!text.contains("PRIVATE-"));
        if query.starts_with("error=") {
            assert_eq!(body.as_ref(), DENIED);
        } else {
            assert!(text.contains("Callback rejected ("));
        }
    }
}
