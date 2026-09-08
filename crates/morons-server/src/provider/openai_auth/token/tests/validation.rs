use super::*;

fn rejected(bytes: &[u8], reason: Reason) {
    let error = parse_tokens(bytes, 1000).unwrap_err();
    assert_eq!(error, OAuthError::InvalidTokenResponse(reason));
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(text.len() < 160);
        assert!(!text.contains("PRIVATE"));
        assert!(!text.contains("fixture"));
        assert!(!text.contains("4600"));
    }
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn extensions_have_strict_duplicate_name_count_body_and_depth_bounds() {
    let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
    for index in 0..26 {
        envelope[format!("extension{index}")] = json!(null);
    }
    assert_eq!(envelope.as_object().unwrap().len(), 32);
    assert!(parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).is_ok());
    envelope["overflow"] = json!("PRIVATE");
    rejected(&serde_json::to_vec(&envelope).unwrap(), Reason::TokenFields);

    for (key, valid) in [
        ("a".repeat(128), true),
        ("λ".repeat(64), true),
        ("a".repeat(129), false),
        (String::new(), false),
    ] {
        let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
        envelope[key] = json!("PRIVATE");
        let bytes = serde_json::to_vec(&envelope).unwrap();
        if valid {
            assert!(parse_tokens(&bytes, 1000).is_ok());
        } else {
            rejected(&bytes, Reason::TokenFields);
        }
    }
    for key in ["error", "error_description", "error_uri"] {
        let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
        envelope[key] = json!("PRIVATE");
        rejected(&serde_json::to_vec(&envelope).unwrap(), Reason::TokenFields);
    }
    let mut bytes = response(1000);
    bytes.resize(MAX_TOKEN_BODY, b' ');
    assert!(parse_tokens(&bytes, 1000).is_ok());
    bytes.push(b' ');
    rejected(&bytes, Reason::BodyBounds);
    let base = String::from_utf8(response(1000)).unwrap();
    for extra in [
        r#""PRIVATE":1,"PRIVATE":2"#,
        r#""PRIVATE":1,"\u0050RIVATE":2"#,
        r#""PRIVATE":{"x":1,"x":2}"#,
        r#""PRIVATE":[{"x":1,"x":2}]"#,
        r#""access_\u0074oken":"PRIVATE""#,
    ] {
        rejected(format!("{{{extra},{}", &base[1..]).as_bytes(), Reason::Json);
    }
    rejected(
        format!(
            "{{\"PRIVATE\":{}0{},{}}}",
            "[".repeat(130),
            "]".repeat(130),
            &base[1..base.len() - 1]
        )
        .as_bytes(),
        Reason::Json,
    );
    rejected(b"{\"PRIVATE\":", Reason::Json);
    rejected(b"[]", Reason::TokenFields);
}

#[test]
fn rejection_reasons_distinguish_claims_and_time_without_exposing_values() {
    for (claims, reason) in [
        ("not-json-PRIVATE", Reason::ClaimsJson),
        (r#"{"exp":1,"exp":2}"#, Reason::ClaimsJson),
        (r#"{"PRIVATE":1}"#, Reason::ClaimExpiry),
        (
            r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":"PRIVATE\n"}}"#,
            Reason::AccountClaim,
        ),
        (
            r#"{"exp":1300,"https://api.openai.com/auth":{"chatgpt_account_id":"PRIVATE"}}"#,
            Reason::EffectiveLifetime,
        ),
    ] {
        rejected(&with_claims(claims), reason);
    }
    let claims = json!({"exp":1000+MAX_TOKEN_LIFETIME_SECONDS+1,"https://api.openai.com/auth":{"chatgpt_account_id":"PRIVATE"}});
    rejected(&with_claims(&claims.to_string()), Reason::EffectiveLifetime);
    let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
    envelope["access_token"] = json!("PRIVATE.not-base64!.signature");
    rejected(
        &serde_json::to_vec(&envelope).unwrap(),
        Reason::AccessTokenFormat,
    );
    for field in ["access_token", "refresh_token", "expires_in"] {
        let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
        envelope.as_object_mut().unwrap().remove(field);
        rejected(
            &serde_json::to_vec(&envelope).unwrap(),
            if field == "expires_in" {
                Reason::ResponseLifetime
            } else {
                Reason::TokenFields
            },
        );
    }
    let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
    for field in ["token_type", "scope", "id_token"] {
        envelope.as_object_mut().unwrap().remove(field);
    }
    assert!(parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).is_ok());
}
