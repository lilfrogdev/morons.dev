use super::invalid;
use crate::{persistence::PersistenceError, provider::openai_auth::OAuthTokens};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

pub(in crate::persistence::credentials) const MAX_FILE: usize = 64 * 1024;
const PREFIX: &[u8] = b"morons.dev/openai-chatgpt-credential/v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Phase {
    Removed = 0,
    Active = 1,
    Dispatched = 2,
    Reauthentication = 3,
}

pub(super) struct State {
    pub generation: u64,
    pub revision: u64,
    pub mutation: [u8; 16],
    pub refresh: [u8; 16],
    pub phase: Phase,
    pub tokens: Option<OAuthTokens>,
}
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenAiCredentialState([REDACTED])")
    }
}
impl State {
    pub fn absent() -> Self {
        Self {
            generation: 0,
            revision: 0,
            mutation: [0; 16],
            refresh: [0; 16],
            phase: Phase::Removed,
            tokens: None,
        }
    }
    fn validate(&self) -> Result<(), PersistenceError> {
        if self.generation == 0
            || self.generation > i64::MAX as u64
            || self.mutation == [0; 16]
            || self.revision > i64::MAX as u64
        {
            return Err(invalid());
        }
        match self.phase {
            Phase::Removed
                if self.tokens.is_none() && self.revision == 0 && self.refresh == [0; 16] =>
            {
                Ok(())
            }
            Phase::Active
                if self.tokens.is_some()
                    && ((self.revision == 1 && self.refresh == [0; 16])
                        || (self.revision > 1 && self.refresh != [0; 16])) =>
            {
                Ok(())
            }
            Phase::Dispatched | Phase::Reauthentication
                if self.tokens.is_some() && self.revision > 0 && self.refresh != [0; 16] =>
            {
                Ok(())
            }
            _ => Err(invalid()),
        }
    }
}

pub(super) fn encode(state: &State) -> Result<Zeroizing<Vec<u8>>, PersistenceError> {
    state.validate()?;
    let (access, refresh, account, expires) = state
        .tokens
        .as_ref()
        .map_or(("", "", "", 0), OAuthTokens::stored_parts);
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        PREFIX.len() + 128 + access.len() + refresh.len() + account.len(),
    ));
    bytes.extend_from_slice(PREFIX);
    bytes.push(state.phase as u8);
    bytes.extend_from_slice(&state.generation.to_be_bytes());
    bytes.extend_from_slice(&state.revision.to_be_bytes());
    bytes.extend_from_slice(&state.mutation);
    bytes.extend_from_slice(&state.refresh);
    bytes.extend_from_slice(&expires.to_be_bytes());
    for text in [access, refresh, account] {
        bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
        bytes.extend_from_slice(text.as_bytes());
    }
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    if bytes.len() > MAX_FILE {
        return Err(invalid());
    }
    Ok(bytes)
}

pub(super) fn decode(bytes: &[u8]) -> Result<State, PersistenceError> {
    if bytes.len() > MAX_FILE || bytes.len() < PREFIX.len() + 1 + 8 + 8 + 16 + 16 + 8 + 12 + 32 {
        return Err(invalid());
    }
    let (body, digest) = bytes.split_at(bytes.len() - 32);
    if Sha256::digest(body).as_slice() != digest {
        return Err(invalid());
    }
    let mut reader = Reader(body.strip_prefix(PREFIX).ok_or_else(invalid)?);
    let phase = match reader.take(1)?[0] {
        0 => Phase::Removed,
        1 => Phase::Active,
        2 => Phase::Dispatched,
        3 => Phase::Reauthentication,
        _ => return Err(invalid()),
    };
    let generation = u64::from_be_bytes(reader.array()?);
    let revision = u64::from_be_bytes(reader.array()?);
    let mutation = reader.array()?;
    let refresh = reader.array()?;
    let expires = u64::from_be_bytes(reader.array()?);
    let access = reader.text(16384)?;
    let refresh_token = reader.text(16384)?;
    let account = reader.text(128)?;
    if !reader.0.is_empty() {
        return Err(invalid());
    }
    let tokens = if phase == Phase::Removed {
        if !access.is_empty() || !refresh_token.is_empty() || !account.is_empty() || expires != 0 {
            return Err(invalid());
        }
        None
    } else {
        Some(
            OAuthTokens::from_stored(access, refresh_token, account, expires)
                .map_err(|_| invalid())?,
        )
    };
    let state = State {
        generation,
        revision,
        mutation,
        refresh,
        phase,
        tokens,
    };
    state.validate()?;
    Ok(state)
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], PersistenceError> {
        if length > self.0.len() {
            return Err(invalid());
        }
        let (part, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(part)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], PersistenceError> {
        self.take(N)?.try_into().map_err(|_| invalid())
    }
    fn text(&mut self, maximum: usize) -> Result<Zeroizing<String>, PersistenceError> {
        let length = u32::from_be_bytes(self.array()?) as usize;
        if length > maximum {
            return Err(invalid());
        }
        Ok(Zeroizing::new(
            std::str::from_utf8(self.take(length)?)
                .map_err(|_| invalid())?
                .to_owned(),
        ))
    }
}
