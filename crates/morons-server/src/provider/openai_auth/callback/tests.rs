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
        Some(Reply::Code(_))
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
        "code=a&state=expected&unknown=value",
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
            parse_request(request(query).as_bytes(), "localhost:1455", "expected").is_none(),
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
            parse_request(invalid.as_bytes(), "localhost:1455", "expected").is_none(),
            "HTTP classification"
        );
    }
    assert!(matches!(
        parse_request(
            request("error=access_denied&error_description=not+approved&state=expected").as_bytes(),
            "localhost:1455",
            "expected"
        ),
        Some(Reply::Denied)
    ));
    assert!(
        matches!(parse_request(request("code=a%2Bb%3Dc&state=expected").as_bytes(),"localhost:1455","expected"),Some(Reply::Code(code)) if code.as_str()=="a+b=c")
    );
    assert!(
        parse_request(
            request(&format!("code={}&state=expected", "x".repeat(4097))).as_bytes(),
            "localhost:1455",
            "expected"
        )
        .is_none()
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
        assert_eq!(response, REJECTED);
    }
    assert_eq!(task.await.unwrap().unwrap_err(), OAuthError::CallbackLimit);
    assert!(TcpStream::connect(address).await.is_err());
}
