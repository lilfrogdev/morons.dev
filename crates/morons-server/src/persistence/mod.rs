mod backend;
mod credentials;
mod database;
mod paths;
mod types;
mod workspace;

use std::{path::Path, thread};

use morons_protocol::ServerEndpoint;
use tokio::sync::{mpsc, oneshot};

use self::{
    backend::Backend,
    credentials::StoredOpenCodeApiKey,
    types::{
        MAX_SESSION_CATALOG_EVENT_PAGE_SIZE, MAX_SESSION_PAGE_SIZE, REQUEST_FINGERPRINT_BYTES,
        create_session_fingerprint, validate_display_name,
    },
};

pub use self::types::{
    MutationRequestId, OpenCodeCredentialStatus, PersistenceError, PersistenceResourceLimit,
    Session, SessionCatalogEvent, SessionCatalogEventCursor, SessionCatalogEventPage, SessionId,
    SessionListCursor, SessionPage,
};

const WORKER_QUEUE_CAPACITY: usize = 64;

pub struct SessionStore {
    sender: Option<mpsc::Sender<WorkerRequest>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SessionStore {
    pub fn open(server: &ServerEndpoint) -> Result<Self, PersistenceError> {
        Self::open_at(server.claim_persistence_root()?)
    }

    fn open_at(application_root: &Path) -> Result<Self, PersistenceError> {
        let backend = Backend::open(application_root)?;
        let (sender, receiver) = mpsc::channel(WORKER_QUEUE_CAPACITY);
        let worker = thread::Builder::new()
            .name("morons-persistence".to_owned())
            .spawn(move || run_worker(backend, receiver))?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    pub async fn create_session(
        &self,
        request_id: MutationRequestId,
        display_name: Option<String>,
    ) -> Result<Session, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        validate_display_name(display_name.as_deref())?;
        let fingerprint = create_session_fingerprint(display_name.as_deref());
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CreateSession {
                request_id,
                fingerprint,
                display_name,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn open_code_credential_status(
        &self,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetOpenCodeCredentialStatus {
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn set_open_code_credential(
        &self,
        request_id: MutationRequestId,
        expected_generation: u64,
        api_key: Vec<u8>,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let api_key = StoredOpenCodeApiKey::new(api_key)?;
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::SetOpenCodeCredential {
                request_id,
                expected_generation,
                api_key,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn remove_open_code_credential(
        &self,
        request_id: MutationRequestId,
        expected_generation: u64,
    ) -> Result<OpenCodeCredentialStatus, PersistenceError> {
        if request_id.is_zero() {
            return Err(PersistenceError::InvalidInput {
                reason: "a mutation request identifier must not be all zeroes",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::RemoveOpenCodeCredential {
                request_id,
                expected_generation,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn get_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Session>, PersistenceError> {
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetSession {
                session_id,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn list_sessions(
        &self,
        cursor: Option<SessionListCursor>,
        limit: u16,
    ) -> Result<SessionPage, PersistenceError> {
        if limit == 0 || limit > MAX_SESSION_PAGE_SIZE {
            return Err(PersistenceError::InvalidInput {
                reason: "a session page size must be between 1 and 100",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ListSessions {
                cursor,
                limit,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    pub async fn read_session_catalog_events(
        &self,
        cursor: SessionCatalogEventCursor,
        limit: u16,
    ) -> Result<SessionCatalogEventPage, PersistenceError> {
        if limit == 0 || limit > MAX_SESSION_CATALOG_EVENT_PAGE_SIZE {
            return Err(PersistenceError::InvalidInput {
                reason: "a session event page size must be between 1 and 100",
            });
        }
        let (response_sender, response_receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::ReadSessionCatalogEvents {
                cursor,
                limit,
                response: response_sender,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        response_receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    fn sender(&self) -> Result<&mpsc::Sender<WorkerRequest>, PersistenceError> {
        self.sender.as_ref().ok_or(PersistenceError::WorkerStopped)
    }
}

impl Drop for SessionStore {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum WorkerRequest {
    CreateSession {
        request_id: MutationRequestId,
        fingerprint: [u8; REQUEST_FINGERPRINT_BYTES],
        display_name: Option<String>,
        response: oneshot::Sender<Result<Session, PersistenceError>>,
    },
    GetOpenCodeCredentialStatus {
        response: oneshot::Sender<Result<OpenCodeCredentialStatus, PersistenceError>>,
    },
    SetOpenCodeCredential {
        request_id: MutationRequestId,
        expected_generation: u64,
        api_key: StoredOpenCodeApiKey,
        response: oneshot::Sender<Result<OpenCodeCredentialStatus, PersistenceError>>,
    },
    RemoveOpenCodeCredential {
        request_id: MutationRequestId,
        expected_generation: u64,
        response: oneshot::Sender<Result<OpenCodeCredentialStatus, PersistenceError>>,
    },
    GetSession {
        session_id: SessionId,
        response: oneshot::Sender<Result<Option<Session>, PersistenceError>>,
    },
    ListSessions {
        cursor: Option<SessionListCursor>,
        limit: u16,
        response: oneshot::Sender<Result<SessionPage, PersistenceError>>,
    },
    ReadSessionCatalogEvents {
        cursor: SessionCatalogEventCursor,
        limit: u16,
        response: oneshot::Sender<Result<SessionCatalogEventPage, PersistenceError>>,
    },
}

fn run_worker(mut backend: Backend, mut receiver: mpsc::Receiver<WorkerRequest>) {
    while let Some(request) = receiver.blocking_recv() {
        match request {
            WorkerRequest::CreateSession {
                request_id,
                fingerprint,
                display_name,
                response,
            } => {
                let _ =
                    response.send(backend.create_session(request_id, fingerprint, display_name));
            }
            WorkerRequest::GetOpenCodeCredentialStatus { response } => {
                let _ = response.send(backend.open_code_credential_status());
            }
            WorkerRequest::SetOpenCodeCredential {
                request_id,
                expected_generation,
                api_key,
                response,
            } => {
                let _ = response.send(backend.set_open_code_credential(
                    request_id,
                    expected_generation,
                    api_key,
                ));
            }
            WorkerRequest::RemoveOpenCodeCredential {
                request_id,
                expected_generation,
                response,
            } => {
                let _ = response
                    .send(backend.remove_open_code_credential(request_id, expected_generation));
            }
            WorkerRequest::GetSession {
                session_id,
                response,
            } => {
                let _ = response.send(backend.get_session(session_id));
            }
            WorkerRequest::ListSessions {
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.list_sessions(cursor, limit));
            }
            WorkerRequest::ReadSessionCatalogEvents {
                cursor,
                limit,
                response,
            } => {
                let _ = response.send(backend.read_session_catalog_events(cursor, limit));
            }
        }
    }
}

#[cfg(test)]
mod credential_tests;
#[cfg(test)]
mod tests;
