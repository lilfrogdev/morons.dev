use super::{Rejection, Reply};
use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use zeroize::Zeroizing;

const MAX_FIELDS: usize = 16;
const MAX_NAME: usize = 128;
const MAX_VALUE: usize = 4096;
const ISSUER: &str = "https://auth.openai.com";

pub(super) fn parse_query(query: &str, expected_state: &str) -> Result<Reply, Rejection> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    let mut issuer = None;
    let mut names = Vec::new();
    for (index, field) in query.split('&').enumerate() {
        if index >= MAX_FIELDS {
            return Err(Rejection::QueryFormat);
        }
        let (key, value) = field.split_once('=').ok_or(Rejection::QueryFormat)?;
        let key = decode_component(key).ok_or(Rejection::QueryFormat)?;
        let value = decode_component(value).ok_or(Rejection::QueryFormat)?;
        if key.is_empty() || key.len() > MAX_NAME || value.len() > MAX_VALUE {
            return Err(Rejection::QueryFormat);
        }
        if names.contains(&key) {
            return Err(Rejection::DuplicateField);
        }
        match key.as_str() {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "error" => error = Some(value),
            "error_description" => description = Some(value),
            "iss" => issuer = Some(value),
            "access_token" | "refresh_token" | "id_token" => return Err(Rejection::ResponseShape),
            // RFC6749 4.1.2: response extensions are not authorization or routing facts.
            // Decode and bound them, reject duplicates, then discard their values.
            _ => {}
        }
        names.push(key);
    }
    let state = state.ok_or(Rejection::State)?;
    if state.len() != expected_state.len() || !state_matches(&state, expected_state) {
        return Err(Rejection::State);
    }
    if issuer
        .as_ref()
        .is_some_and(|value| value.as_str() != ISSUER)
    {
        return Err(Rejection::Issuer);
    }
    match (code, error, description) {
        (Some(code), None, None)
            if !code.is_empty() && code.bytes().all(|c| (0x21..=0x7e).contains(&c)) =>
        {
            Ok(Reply::Code(code))
        }
        (None, Some(error), description)
            if !error.is_empty()
                && error.len() <= 128
                && description.as_ref().is_none_or(|s| s.len() <= 2048) =>
        {
            Ok(Reply::Denied)
        }
        _ => Err(Rejection::ResponseShape),
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
pub(in crate::provider::openai_auth) fn decode_component(value: &str) -> Option<Zeroizing<String>> {
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
            // Legal query literals; '&' separates fields and '#' never enters a query.
            b'!' | b'$' | b'\'' | b'(' | b')' | b'*' | b',' | b';' | b'=' | b':' | b'@' | b'/'
            | b'?' => byte,
            _ => return None,
        };
        if !(0x20..=0x7e).contains(&byte) {
            return None;
        }
        bytes.push(byte);
    }
    Some(Zeroizing::new(std::str::from_utf8(&bytes).ok()?.to_owned()))
}
