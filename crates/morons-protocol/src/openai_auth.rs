use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::fmt;
use zeroize::Zeroize;

/// Ephemeral browser interaction only; never transcript, ordinary status or logging data.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenAiAuthorizationUrl(String);
impl OpenAiAuthorizationUrl {
    pub fn new(mut value: String) -> Result<Self, &'static str> {
        if value.len() > 4096
            || !value.starts_with("https://auth.openai.com/oauth/authorize?")
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.~:/?=%&".contains(&b))
        {
            value.zeroize();
            return Err("invalid OpenAI browser URL");
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Drop for OpenAiAuthorizationUrl {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl fmt::Debug for OpenAiAuthorizationUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpenAiAuthorizationUrl([REDACTED])")
    }
}
impl Serialize for OpenAiAuthorizationUrl {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}
impl<'de> Deserialize<'de> for OpenAiAuthorizationUrl {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(d)?).map_err(de::Error::custom)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiCredentialState {
    Unconfigured,
    Configured,
    ReauthenticationRequired,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenAiCredentialStatus {
    pub generation: u64,
    pub state: OpenAiCredentialState,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenAiLoginFailure {
    Busy,
    CallbackUnavailable,
    Denied,
    Expired,
    ExchangeRejected,
    ExchangeUncertain,
    InvalidResponse,
    Unavailable,
    CredentialChanged,
    InstallationUncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenAiLoginResult {
    Installed { generation: u64 },
    CancelledBeforeInstallation,
    Failed { failure: OpenAiLoginFailure },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApplicationRequest, ApplicationResponse, ClientMessage, MutationRequestId, ServerMessage,
    };
    #[test]
    fn authentication_wire_is_closed_bounded_and_redacted() {
        let url = OpenAiAuthorizationUrl::new(
            "https://auth.openai.com/oauth/authorize?state=synthetic-state".into(),
        )
        .unwrap();
        let message = ServerMessage::response(
            1,
            ApplicationResponse::OpenAiLoginStarted {
                attempt_id: MutationRequestId::from_bytes([1; 16]),
                url,
            },
        );
        assert!(!format!("{message:?}").contains("synthetic-state"));
        assert_eq!(
            ServerMessage::decode_json(&message.encode_json().unwrap()).unwrap(),
            message
        );
        for value in [
            "https://evil.invalid/",
            "https://auth.openai.com/oauth/authorize?state=\x1b",
            "https://auth.openai.com/oauth/authorize?state=x#evil",
        ] {
            assert!(OpenAiAuthorizationUrl::new(value.into()).is_err());
        }
        assert!(
            OpenAiAuthorizationUrl::new(format!(
                "https://auth.openai.com/oauth/authorize?{}",
                "a".repeat(4096)
            ))
            .is_err()
        );
        let request = ClientMessage::request(
            1,
            ApplicationRequest::BeginOpenAiLogin {
                mutation_request_id: MutationRequestId::from_bytes([1; 16]),
                expected_generation: 0,
            },
        );
        let mut value = serde_json::to_value(request).unwrap();
        value["request"]["access_token"] = serde_json::json!("not-accepted");
        assert!(serde_json::from_value::<ClientMessage>(value).is_err());
        assert!(
            serde_json::from_str::<OpenAiCredentialStatus>(
                r#"{"generation":1,"state":"configured","account":"not-exposed"}"#
            )
            .is_err()
        );
    }
}
