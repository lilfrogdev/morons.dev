pub(in crate::persistence::credentials) mod format;

use super::files;
use crate::{
    persistence::{
        CredentialIdentityStatus, CredentialKind, OpenAiCredentialState, OpenAiCredentialStatus,
        PersistenceError, PersistenceResourceLimit, paths::path_entry_exists,
    },
    provider::openai_auth::{OAuthRefreshGrant, OAuthTokens, OpenAiAuthorization},
};
use format::{Phase, State};
use std::path::{Path, PathBuf};

pub(in crate::persistence) struct OpenAiCredentialStore {
    directory: PathBuf,
    state: State,
    consistent: bool,
}

pub(in crate::persistence) enum OpenAiAccess {
    Fresh(OpenAiAuthorization),
    Refresh {
        marker: [u8; 16],
        grant: OAuthRefreshGrant,
    },
}

pub(in crate::persistence::credentials) fn invalid() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "OpenAI credential state is invalid or inconsistent",
    }
}

impl OpenAiCredentialStore {
    pub(in crate::persistence) fn open(root: &Path) -> Result<Self, PersistenceError> {
        let directory = root.join("credentials");
        let path = files::path(&directory, CredentialKind::OpenAiChatGpt);
        let state = if path_entry_exists(&path)? {
            format::decode(&files::read(&path, format::MAX_FILE)?)?
        } else {
            State::absent()
        };
        Ok(Self {
            directory,
            state,
            consistent: true,
        })
    }
    pub(in crate::persistence) fn ensure_consistent(&self) -> Result<(), PersistenceError> {
        if !self.consistent {
            return Err(invalid());
        }
        let path = files::path(&self.directory, CredentialKind::OpenAiChatGpt);
        if self.state.generation == 0 {
            if path_entry_exists(&path)? {
                return Err(invalid());
            }
        } else if files::read(&path, format::MAX_FILE)?.as_slice()
            != format::encode(&self.state)?.as_slice()
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(in crate::persistence) fn is_consistent(&self) -> bool {
        self.consistent
    }
    pub(in crate::persistence) fn identity(&self) -> CredentialIdentityStatus {
        CredentialIdentityStatus {
            configured: self.state.phase != Phase::Removed,
            generation: self.state.generation,
        }
    }
    pub(in crate::persistence) fn marker(&self) -> &[u8; 16] {
        &self.state.mutation
    }
    pub(in crate::persistence) fn status(&self) -> OpenAiCredentialStatus {
        OpenAiCredentialStatus {
            generation: self.state.generation,
            state: match self.state.phase {
                Phase::Removed => OpenAiCredentialState::Unconfigured,
                Phase::Active | Phase::Dispatched => OpenAiCredentialState::Configured,
                Phase::Reauthentication => OpenAiCredentialState::ReauthenticationRequired,
            },
        }
    }
    pub(in crate::persistence) fn apply(
        &mut self,
        expected: u64,
        marker: [u8; 16],
        tokens: Option<OAuthTokens>,
        now: u64,
    ) -> Result<CredentialIdentityStatus, PersistenceError> {
        self.ensure_consistent()?;
        self.generation(expected)?;
        if marker == [0; 16] {
            return Err(invalid());
        }
        if tokens
            .as_ref()
            .is_some_and(|t| t.expires_at_seconds().saturating_sub(now) <= 300)
        {
            return Err(PersistenceError::CredentialReauthenticationRequired);
        }
        let generation = increment(expected)?;
        let configured = tokens.is_some();
        self.state = State {
            generation,
            revision: u64::from(configured),
            mutation: marker,
            refresh: [0; 16],
            phase: if configured {
                Phase::Active
            } else {
                Phase::Removed
            },
            tokens,
        };
        self.save()?;
        Ok(self.identity())
    }
    pub(in crate::persistence) fn recover_refresh(&mut self) -> Result<(), PersistenceError> {
        self.ensure_consistent()?;
        if self.state.phase == Phase::Dispatched {
            self.state.phase = Phase::Reauthentication;
            self.save()?;
        }
        Ok(())
    }
    pub(in crate::persistence) fn begin_access(
        &mut self,
        expected: u64,
        marker: [u8; 16],
        now: u64,
    ) -> Result<OpenAiAccess, PersistenceError> {
        self.ensure_consistent()?;
        self.generation(expected)?;
        self.recover_refresh()?;
        match self.state.phase {
            Phase::Removed => return Err(PersistenceError::CredentialNotConfigured),
            Phase::Reauthentication => {
                return Err(PersistenceError::CredentialReauthenticationRequired);
            }
            Phase::Dispatched => return Err(invalid()),
            Phase::Active => {}
        }
        let tokens = self.state.tokens.as_ref().ok_or_else(invalid)?;
        if tokens.expires_at_seconds().saturating_sub(now) > 300 {
            return Ok(OpenAiAccess::Fresh(tokens.authorization()));
        }
        increment(self.state.revision)?;
        if marker == [0; 16] || marker == self.state.refresh {
            return Err(invalid());
        }
        self.state.phase = Phase::Dispatched;
        self.state.refresh = marker;
        self.save()?;
        Ok(OpenAiAccess::Refresh {
            marker,
            grant: self
                .state
                .tokens
                .as_ref()
                .ok_or_else(invalid)?
                .refresh_grant(),
        })
    }
    pub(in crate::persistence) fn finish_refresh(
        &mut self,
        expected: u64,
        marker: [u8; 16],
        result: Option<OAuthTokens>,
        now: u64,
    ) -> Result<OpenAiAuthorization, PersistenceError> {
        self.ensure_consistent()?;
        self.generation(expected)?;
        if self.state.phase != Phase::Dispatched || marker != self.state.refresh {
            return Err(invalid());
        }
        let valid = result.as_ref().is_some_and(|next| {
            self.state
                .tokens
                .as_ref()
                .is_some_and(|old| old.same_account(next))
                && next.expires_at_seconds().saturating_sub(now) > 300
        });
        if !valid {
            self.state.phase = Phase::Reauthentication;
            self.save()?;
            return Err(PersistenceError::CredentialReauthenticationRequired);
        }
        self.state.revision = increment(self.state.revision)?;
        self.state.phase = Phase::Active;
        self.state.tokens = result;
        self.save()?;
        Ok(self
            .state
            .tokens
            .as_ref()
            .ok_or_else(invalid)?
            .authorization())
    }
    fn generation(&self, expected: u64) -> Result<(), PersistenceError> {
        if self.state.generation != expected {
            Err(PersistenceError::CredentialGenerationConflict)
        } else {
            Ok(())
        }
    }
    fn save(&mut self) -> Result<(), PersistenceError> {
        self.consistent = false;
        let mut temporary = [0_u8; 16];
        getrandom::fill(&mut temporary).map_err(|_| invalid())?;
        let payload = format::encode(&self.state)?;
        files::replace(
            &self.directory,
            CredentialKind::OpenAiChatGpt,
            &temporary,
            &payload,
        )?;
        self.consistent = true;
        Ok(())
    }
}
fn increment(value: u64) -> Result<u64, PersistenceError> {
    value
        .checked_add(1)
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or(PersistenceError::ResourceLimit {
            resource: PersistenceResourceLimit::CredentialGeneration,
        })
}

#[cfg(test)]
mod tests;
