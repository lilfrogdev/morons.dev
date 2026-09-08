use crate::{connect_or_start, generate_mutation_request_id};
use morons_protocol::{
    ApplicationError, ApplicationRequest as Request, ApplicationResponse as Response,
    ClientMessage, MutationRequestId, OpenAiLoginResult, ServerMessage, read_server_message,
    write_client_message,
};
use std::time::Duration;
#[cfg(test)]
mod tests;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time,
};

pub(super) enum Command {
    Status,
    Begin(u64),
    Remove(u64),
}
pub(super) use crate::app::auth::AuthEvent;
use crate::app::auth::{UNKNOWN, failure_message};
pub(super) struct AuthRuntime {
    pub(super) events: mpsc::Receiver<AuthEvent>,
    sender: mpsc::Sender<AuthEvent>,
    task: Option<JoinHandle<()>>,
    cancel: Option<watch::Sender<bool>>,
}
impl Default for AuthRuntime {
    fn default() -> Self {
        let (sender, events) = mpsc::channel(4);
        Self {
            events,
            sender,
            task: None,
            cancel: None,
        }
    }
}
impl Drop for AuthRuntime {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl AuthRuntime {
    pub(super) async fn start(&mut self, command: Command) -> bool {
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return false;
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        let (cancel, cancellation) = watch::channel(false);
        self.cancel = Some(cancel);
        let events = self.sender.clone();
        self.task = Some(tokio::spawn(async move {
            let result = time::timeout(
                Duration::from_secs(680),
                execute(command, &events, cancellation),
            )
            .await;
            if !matches!(result, Ok(Ok(()))) {
                let _ = events.try_send(AuthEvent::Failed(UNKNOWN));
            }
        }));
        true
    }
    pub(super) fn cancel(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.send_replace(true);
        }
    }
    pub(super) async fn shutdown(&mut self) {
        self.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
async fn execute(
    command: Command,
    events: &mpsc::Sender<AuthEvent>,
    cancel: watch::Receiver<bool>,
) -> Result<(), ()> {
    let connected = time::timeout(Duration::from_secs(10), connect_or_start())
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    if *cancel.borrow() {
        return send(
            events,
            AuthEvent::Finished(OpenAiLoginResult::CancelledBeforeInstallation),
        );
    }
    exchange(&mut connected.into_connection(), command, events, cancel).await
}
async fn exchange<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut connection: &mut S,
    command: Command,
    events: &mpsc::Sender<AuthEvent>,
    mut cancel: watch::Receiver<bool>,
) -> Result<(), ()> {
    let expected = match command {
        Command::Begin(g) | Command::Remove(g) => g.checked_add(1),
        Command::Status => None,
    };
    let id = generate_mutation_request_id().map_err(|_| ())?;
    let login = matches!(command, Command::Begin(_));
    let request = match command {
        Command::Status => Request::GetOpenAiCredentialStatus,
        Command::Begin(generation) => Request::BeginOpenAiLogin {
            mutation_request_id: id,
            expected_generation: generation,
        },
        Command::Remove(generation) => Request::RemoveOpenAiCredential {
            mutation_request_id: id,
            expected_generation: generation,
        },
    };
    time::timeout(
        Duration::from_secs(5),
        write_client_message(&mut connection, &ClientMessage::request(1, request)),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())?;
    let response = tokio::select! {
        biased;
        _=cancel.changed()=>return Err(()),
        result=time::timeout(Duration::from_secs(70),read_server_message(&mut connection))=>result.map_err(|_|())?.map_err(|_|())?.ok_or(())?,
    };
    match response {
        ServerMessage::Response {
            request_id: 1,
            response: Response::OpenAiCredentialStatus { credential },
        } if !login
            && credential.generation <= i64::MAX as u64
            && (credential.state == morons_protocol::OpenAiCredentialState::Unconfigured
                || credential.generation > 0)
            && expected.is_none_or(|g| {
                credential.generation == g
                    && credential.state == morons_protocol::OpenAiCredentialState::Unconfigured
            }) =>
        {
            send(events, AuthEvent::Status(credential))
        }
        ServerMessage::Response {
            request_id: 1,
            response: Response::OpenAiLoginStarted { attempt_id, url },
        } if login && attempt_id == id => {
            if !*cancel.borrow() {
                send(events, AuthEvent::Started(url))?;
            }
            let (mut reader, mut writer) = tokio::io::split(&mut connection);
            let finished = read_server_message(&mut reader);
            tokio::pin!(finished);
            let response = if *cancel.borrow_and_update() {
                write_cancel(&mut writer, id).await?;
                time::timeout(Duration::from_secs(70), &mut finished)
                    .await
                    .map_err(|_| ())?
            } else {
                tokio::select! {
                    result=&mut finished=>result,
                    _=cancel.changed()=> {
                        write_cancel(&mut writer,id).await?;
                        time::timeout(Duration::from_secs(70),&mut finished).await.map_err(|_|())?
                    }
                }
            }
            .map_err(|_| ())?;
            match response {
                Some(ServerMessage::OpenAiLoginFinished {
                    attempt_id,
                    outcome,
                }) if attempt_id == id
                    && match outcome {
                        OpenAiLoginResult::Installed { generation } => {
                            Some(generation) == expected && generation <= i64::MAX as u64
                        }
                        _ => true,
                    } =>
                {
                    send(events, AuthEvent::Finished(outcome))
                }
                _ => Err(()),
            }
        }
        ServerMessage::RequestFailed {
            request_id: 1,
            error,
        } => send(
            events,
            AuthEvent::Failed(match error {
                ApplicationError::OpenAiLoginFailed { failure } => failure_message(failure),
                ApplicationError::CredentialGenerationConflict => {
                    "ChatGPT credential changed. Reopen /login to reload; nothing was retried."
                }
                _ => {
                    "Authentication request rejected. Reopen /login to reload status; nothing was retried."
                }
            }),
        ),
        _ => Err(()),
    }
}
async fn write_cancel<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    id: MutationRequestId,
) -> Result<(), ()> {
    time::timeout(
        Duration::from_secs(5),
        write_client_message(
            writer,
            &ClientMessage::request(2, Request::CancelOpenAiLogin { attempt_id: id }),
        ),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}
fn send(events: &mpsc::Sender<AuthEvent>, event: AuthEvent) -> Result<(), ()> {
    events.try_send(event).map_err(|_| ())
}
