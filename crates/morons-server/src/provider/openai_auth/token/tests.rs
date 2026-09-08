mod validation;

use super::*;
use crate::provider::openai_auth::{MAX_TOKEN_LIFETIME_SECONDS, SCOPE};
use serde_json::json;

pub(in crate::provider::openai_auth) fn response(now: u64) -> Vec<u8> {
    let claims = json!({"exp":now+3600,"https://api.openai.com/auth":{"chatgpt_account_id":"account-fixture"}});
    with_claims(&claims.to_string())
}
fn with_claims(claims: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "access_token":format!("header.{}.signature",URL_SAFE_NO_PAD.encode(claims)),
        "refresh_token":"refresh-fixture-do-not-log", "expires_in":3600,
        "token_type":"Bearer", "scope":SCOPE, "id_token":"id-fixture-do-not-log"
    }))
    .unwrap()
}

#[test]
fn token_envelope_required_fields_are_bounded_and_redacted() {
    let bytes = response(1000);
    let tokens = parse_tokens(&bytes, 1000).unwrap();
    assert_eq!(tokens.expires_at_seconds(), 4600);
    assert_eq!(tokens.account.0.as_str(), "account-fixture");
    let debug = format!("{tokens:?}");
    for value in [
        "header.",
        "refresh-fixture",
        "id-fixture",
        "account-fixture",
        "4600",
    ] {
        assert!(!debug.contains(value));
    }
    for (field, value, reason) in [
        ("expires_in", json!(-1), Reason::ResponseLifetime),
        ("expires_in", json!(1.5), Reason::ResponseLifetime),
        ("expires_in", json!(300), Reason::ResponseLifetime),
        (
            "expires_in",
            json!(MAX_TOKEN_LIFETIME_SECONDS + 1),
            Reason::ResponseLifetime,
        ),
        ("expires_in", json!("3600"), Reason::ResponseLifetime),
        ("access_token", json!(""), Reason::TokenFields),
        ("refresh_token", json!("bad\nvalue"), Reason::TokenFields),
        (
            "refresh_token",
            json!("x".repeat(MAX_SECRET + 1)),
            Reason::TokenFields,
        ),
        ("token_type", json!("Basic"), Reason::TokenType),
        ("token_type", Value::Null, Reason::TokenType),
        ("id_token", Value::Null, Reason::TokenFields),
        ("scope", Value::Null, Reason::Scope),
        (
            "scope",
            json!(format!("{SCOPE} api.connectors.invoke")),
            Reason::Scope,
        ),
        ("scope", json!("openid profile email email"), Reason::Scope),
    ] {
        let mut envelope: Value = serde_json::from_slice(&bytes).unwrap();
        envelope[field] = value;
        assert_eq!(
            parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).unwrap_err(),
            OAuthError::InvalidTokenResponse(reason),
            "{field}"
        );
    }
    let duplicate = format!(
        "{{\"expires_in\":3600,{}",
        std::str::from_utf8(&bytes)
            .unwrap()
            .strip_prefix('{')
            .unwrap()
    );
    assert!(parse_tokens(duplicate.as_bytes(), 1000).is_err());
    assert!(parse_tokens(&vec![b' '; MAX_TOKEN_BODY + 1], 1000).is_err());
    assert!(parse_tokens(&bytes, u64::MAX).is_err());
}

#[test]
fn token_response_extensions_do_not_select_routes_accounts_or_policy() {
    let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
    envelope["example_parameter"] = json!("example_value");
    envelope["token_endpoint"] = json!("https://do-not-follow.invalid/token");
    envelope["account"] = json!("do-not-route");
    envelope["metadata"] =
        json!({"model":"do-not-select", "training":"not_used", "retention":"none", "unicode":"λ"});
    let tokens = parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).unwrap();
    assert_eq!(tokens.account.0.as_str(), "account-fixture");
    assert_eq!(tokens.expires_at_seconds(), 4600);
    assert!(!format!("{tokens:?}").contains("do-not-"));
}

#[test]
fn bearer_token_type_is_ascii_case_insensitive_not_a_different_scheme() {
    let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
    for token_type in ["Bearer", "bearer", "BEARER", "bEaReR"] {
        envelope["token_type"] = json!(token_type);
        assert!(parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).is_ok());
    }
    for token_type in ["Basic", " Bearer", "Bearer ", "Bearer\t", "Beаrer"] {
        envelope["token_type"] = json!(token_type);
        assert!(parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).is_err());
    }
}

#[test]
fn claims_are_routing_data_not_jwt_identity_or_policy_verification() {
    for claims in [
        r#"{"exp":4600,"exp":4601,"https://api.openai.com/auth":{"chatgpt_account_id":"account"}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":"a","chatgpt_account_id":"b"}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":"a"},"extra":{"a":1,"a":2}}"#,
        r#"{"exp":1300,"https://api.openai.com/auth":{"chatgpt_account_id":"account"}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":"bad\r\nheader"}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":""}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":1}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{}}"#,
        r#"{"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":"account"}} {}"#,
    ] {
        assert!(parse_tokens(&with_claims(claims), 1000).is_err());
    }
    let mut envelope: Value = serde_json::from_slice(&response(1000)).unwrap();
    for access in ["a.b", "a.b.c.d", "a..c", "a.%FF.c", "a.e30.c"] {
        envelope["access_token"] = json!(access);
        assert!(parse_tokens(&serde_json::to_vec(&envelope).unwrap(), 1000).is_err());
    }
    let earlier = with_claims(
        r#"{"exp":2800,"https://api.openai.com/auth":{"chatgpt_account_id":"account"}}"#,
    );
    assert_eq!(
        parse_tokens(&earlier, 1000).unwrap().expires_at_seconds(),
        2800
    );
    let oversized =
        json!({"exp":4600,"https://api.openai.com/auth":{"chatgpt_account_id":"a".repeat(129)}});
    assert!(parse_tokens(&with_claims(&oversized.to_string()), 1000).is_err());
}
