mod requests;
mod subscriptions;

use std::{error::Error, fmt, fs, io, path::PathBuf};

use morons_protocol::{MutationRequestId, OpenCodeService, SessionId};
use tokio::{sync::mpsc, task::JoinHandle};

use self::{
    requests::{RequestCommand, RequestEvent, SessionSnapshot, run_request_worker},
    subscriptions::{SubscriptionEvent, spawn_catalog_subscription, spawn_session_subscription},
};
use crate::{
    ApplicationClient, ConnectOrStartError, MutationRequestIdError,
    app::{AppAction, AppState, PendingOperation, UiStateError},
    connect_or_start, generate_mutation_request_id,
    terminal::{
        SafeText, TerminalEvents, TerminalInput, TerminalSession, require_interactive_terminal,
    },
};

const REQUEST_COMMAND_CAPACITY: usize = 16;
const REQUEST_EVENT_CAPACITY: usize = 64;
const SUBSCRIPTION_EVENT_CAPACITY: usize = 128;

#[non_exhaustive]
pub enum TerminalApplicationError {
    Terminal(io::Error),
    Connect(ConnectOrStartError),
    MutationIdentifier(MutationRequestIdError),
    State,
    RequestWorkerStopped,
}

impl fmt::Debug for TerminalApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Terminal(_) => "TerminalApplicationError::Terminal",
            Self::Connect(_) => "TerminalApplicationError::Connect",
            Self::MutationIdentifier(_) => "TerminalApplicationError::MutationIdentifier",
            Self::State => "TerminalApplicationError::State",
            Self::RequestWorkerStopped => "TerminalApplicationError::RequestWorkerStopped",
        })
    }
}

impl fmt::Display for TerminalApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Terminal(_) => "terminal application failed",
            Self::Connect(_) => "local server connection failed",
            Self::MutationIdentifier(_) => "mutation identifier generation failed",
            Self::State => "terminal state validation failed",
            Self::RequestWorkerStopped => "local application request worker stopped",
        })
    }
}

impl Error for TerminalApplicationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Terminal(error) => Some(error),
            Self::Connect(error) => Some(error),
            Self::MutationIdentifier(error) => Some(error),
            Self::State => None,
            Self::RequestWorkerStopped => None,
        }
    }
}

impl From<io::Error> for TerminalApplicationError {
    fn from(error: io::Error) -> Self {
        Self::Terminal(error)
    }
}

impl From<ConnectOrStartError> for TerminalApplicationError {
    fn from(error: ConnectOrStartError) -> Self {
        Self::Connect(error)
    }
}

impl From<MutationRequestIdError> for TerminalApplicationError {
    fn from(error: MutationRequestIdError) -> Self {
        Self::MutationIdentifier(error)
    }
}

impl From<UiStateError> for TerminalApplicationError {
    fn from(_error: UiStateError) -> Self {
        Self::State
    }
}

pub async fn run_terminal_application() -> Result<(), TerminalApplicationError> {
    require_interactive_terminal()?;
    let connected = connect_or_start().await?;
    let server_version = connected.server_version().to_owned();
    let client = ApplicationClient::from_negotiated_connection(connected.into_connection());

    let mut terminal = TerminalSession::enter()?;
    let mut terminal_events = TerminalEvents::start()?;
    let (request_commands, request_command_receiver) = mpsc::channel(REQUEST_COMMAND_CAPACITY);
    let (request_events, mut request_event_receiver) = mpsc::channel(REQUEST_EVENT_CAPACITY);
    let request_worker = tokio::spawn(run_request_worker(
        client,
        request_command_receiver,
        request_events,
    ));
    let (subscription_events, mut subscription_event_receiver) =
        mpsc::channel(SUBSCRIPTION_EVENT_CAPACITY);

    let mut runtime = RuntimeState::new(server_version, request_worker);
    enqueue_initial_queries(&request_commands)?;

    let result = loop {
        terminal.draw(|frame| runtime.app.render(frame))?;
        tokio::select! {
            input = terminal_events.next() => {
                let Some(input) = input else {
                    break Err(TerminalApplicationError::Terminal(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "terminal event input ended",
                    )));
                };
                match input? {
                    TerminalInput::Key(key) => {
                        let action = runtime.app.handle_key(key);
                        if runtime
                            .handle_action(action, &request_commands)
                            .await?
                        {
                            break Ok(());
                        }
                    }
                    TerminalInput::Paste(paste) => {
                        if !runtime.app.accepts_image_input() {
                            runtime.app.handle_paste(&paste);
                            continue;
                        }
                        match capture_pasted_image(&paste).await {
                            Some(Ok((image, display_name))) => {
                                runtime.app.add_draft_image(image, Some(&display_name));
                            }
                            Some(Err(error)) => runtime.app.set_status(error),
                            None => runtime.app.handle_paste(&paste),
                        }
                    }
                    TerminalInput::Image(image) => runtime.app.add_draft_image(image, None),
                    TerminalInput::ClipboardUnavailable => {
                        runtime.app.set_status("Clipboard does not contain a supported bounded image or text value");
                    }
                    TerminalInput::Resize => {}
                }
            }
            event = request_event_receiver.recv() => {
                let Some(event) = event else {
                    break Err(TerminalApplicationError::RequestWorkerStopped);
                };
                if runtime
                    .handle_request_event(event, &request_commands, &subscription_events)
                    .await?
                {
                    break Ok(());
                }
            }
            event = subscription_event_receiver.recv() => {
                if let Some(event) = event {
                    runtime.handle_subscription_event(
                        event,
                        &request_commands,
                        &subscription_events,
                    )?;
                }
            }
        }
    };

    drop(request_commands);
    runtime.abort_background_tasks();
    drop(terminal_events);
    terminal.restore()?;
    result
}

struct RuntimeState {
    app: AppState,
    pending_command: Option<RequestCommand>,
    pending_credential_mutation: Option<MutationRequestId>,
    credential_reconciliation_unknown: Option<bool>,
    requested_session: Option<SessionId>,
    session_generation: u64,
    refresh_remaining: u8,
    request_worker: JoinHandle<()>,
    catalog_subscription: Option<JoinHandle<()>>,
    session_subscription: Option<JoinHandle<()>>,
}

impl RuntimeState {
    fn new(server_version: String, request_worker: JoinHandle<()>) -> Self {
        Self {
            app: AppState::new(&server_version),
            pending_command: None,
            pending_credential_mutation: None,
            credential_reconciliation_unknown: None,
            requested_session: None,
            session_generation: 0,
            refresh_remaining: 4,
            request_worker,
            catalog_subscription: None,
            session_subscription: None,
        }
    }

    async fn handle_action(
        &mut self,
        action: AppAction,
        commands: &mpsc::Sender<RequestCommand>,
    ) -> Result<bool, TerminalApplicationError> {
        match action {
            AppAction::None => {}
            AppAction::Quit => return Ok(true),
            AppAction::Refresh => {
                if self.refresh_remaining == 0 {
                    enqueue_initial_queries(commands)?;
                    self.refresh_remaining = 4;
                    if let Some(session_id) = self.requested_session {
                        send_command(commands, RequestCommand::LoadSession(session_id))?;
                    }
                    self.app.set_status("Refreshing local state");
                } else {
                    self.app
                        .set_status("A local-state refresh is already in progress");
                }
            }
            AppAction::CreateSession => {
                let command = RequestCommand::CreateSession {
                    mutation_request_id: generate_mutation_request_id()?,
                };
                self.start_mutation(command, PendingOperation::CreateSession, commands)?;
                self.app.set_status("Creating session");
            }
            AppAction::OpenSession(session_id) => {
                self.requested_session = Some(session_id);
                send_command(commands, RequestCommand::LoadSession(session_id))?;
                self.app.set_status("Loading session transcript");
            }
            AppAction::CloseSession => {
                self.requested_session = None;
                self.session_generation = self.session_generation.wrapping_add(1);
                abort_task(&mut self.session_subscription);
                self.app.close_session();
                self.app
                    .set_status("Session detached; server-owned runs continue");
            }
            AppAction::RenameSession {
                session_id,
                display_name,
            } => {
                let command = RequestCommand::RenameSession {
                    mutation_request_id: generate_mutation_request_id()?,
                    session_id,
                    display_name,
                };
                self.start_mutation(command, PendingOperation::RenameSession, commands)?;
                self.app.set_status("Renaming session");
            }
            AppAction::ShowContext {
                session_id,
                service,
                model_id,
            } => {
                send_command(
                    commands,
                    RequestCommand::LoadContext {
                        session_id,
                        service,
                        model_id,
                    },
                )?;
                self.app.set_status("Calculating approximate context use");
            }
            AppAction::SubmitInput {
                session_id,
                text,
                attachments,
                service,
                model_id,
            } => {
                let command = RequestCommand::SubmitInput {
                    mutation_request_id: generate_mutation_request_id()?,
                    session_id,
                    text,
                    attachments,
                    service,
                    model_id,
                };
                self.start_mutation(command, PendingOperation::SubmitInput, commands)?;
                self.app.set_status("Submitting message");
            }
            AppAction::ExecuteLocalCommand {
                session_id,
                command,
                context_visible,
            } => {
                let command = RequestCommand::ExecuteLocalCommand {
                    mutation_request_id: generate_mutation_request_id()?,
                    session_id,
                    command,
                    context_visible,
                };
                self.start_mutation(command, PendingOperation::ExecuteLocalCommand, commands)?;
                self.app.set_status("Starting local command");
            }
            AppAction::CancelRun { session_id, run_id } => {
                let command = RequestCommand::CancelRun {
                    mutation_request_id: generate_mutation_request_id()?,
                    session_id,
                    run_id,
                };
                self.start_mutation(command, PendingOperation::CancelRun, commands)?;
                self.app.set_status("Requesting exact-run cancellation");
            }
            AppAction::CancelLocalCommand {
                session_id,
                command_id,
            } => {
                let command = RequestCommand::CancelLocalCommand {
                    mutation_request_id: generate_mutation_request_id()?,
                    session_id,
                    command_id,
                };
                self.start_mutation(command, PendingOperation::CancelLocalCommand, commands)?;
                self.app.set_status("Requesting local command cancellation");
            }
            AppAction::AcknowledgeToolUncertainty { session_id, run_id } => {
                let command = RequestCommand::AcknowledgeToolUncertainty {
                    mutation_request_id: generate_mutation_request_id()?,
                    session_id,
                    run_id,
                };
                self.start_mutation(
                    command,
                    PendingOperation::AcknowledgeToolUncertainty,
                    commands,
                )?;
                self.app
                    .set_status("Acknowledging and parking the uncertain workspace effect");
            }
            AppAction::SetCredential {
                expected_generation,
                api_key,
            } => {
                let mutation_request_id = generate_mutation_request_id()?;
                let command = RequestCommand::SetCredential {
                    mutation_request_id,
                    expected_generation,
                    api_key,
                };
                self.start_credential_mutation(mutation_request_id, command, commands)?;
                self.app
                    .set_status("Saving credential without network validation");
            }
            AppAction::RemoveCredential {
                expected_generation,
            } => {
                let mutation_request_id = generate_mutation_request_id()?;
                let command = RequestCommand::RemoveCredential {
                    mutation_request_id,
                    expected_generation,
                };
                self.start_credential_mutation(mutation_request_id, command, commands)?;
                self.app.set_status("Removing stored credential");
            }
            AppAction::StopServer => {
                let command = RequestCommand::StopServer {
                    mutation_request_id: generate_mutation_request_id()?,
                };
                self.start_mutation(command, PendingOperation::StopServer, commands)?;
                self.app.set_status("Requesting graceful server stop");
            }
            AppAction::RetryPending => {
                let command = self
                    .pending_command
                    .as_ref()
                    .and_then(RequestCommand::clone_for_retry)
                    .ok_or(TerminalApplicationError::RequestWorkerStopped)?;
                send_command(commands, command)?;
                if let Some(operation) = self.app.pending {
                    self.app.mark_pending(operation);
                }
                self.app.set_status("Retrying the exact pending mutation");
            }
            AppAction::AbandonPending => {
                self.pending_command = None;
                self.app.clear_pending();
                self.app.set_status(
                    "Pending mutation abandoned; authoritative state was not changed locally",
                );
            }
        }
        Ok(false)
    }

    async fn handle_request_event(
        &mut self,
        event: RequestEvent,
        commands: &mpsc::Sender<RequestCommand>,
        subscription_events: &mpsc::Sender<SubscriptionEvent>,
    ) -> Result<bool, TerminalApplicationError> {
        match event {
            RequestEvent::ConnectionRestored { server_version } => {
                self.app.clear_credential_interaction();
                self.app.server_version = SafeText::from_untrusted(&server_version);
                self.app.replace_models(OpenCodeService::Zen, Vec::new())?;
                self.app.replace_models(OpenCodeService::Go, Vec::new())?;
                self.app.set_status(
                    "Authenticated connection restored; refresh model availability with Ctrl+L",
                );
            }
            RequestEvent::SessionsLoaded { sessions, cursor } => {
                self.complete_refresh_query();
                self.app.replace_sessions(sessions)?;
                abort_task(&mut self.catalog_subscription);
                self.catalog_subscription = Some(spawn_catalog_subscription(
                    cursor,
                    subscription_events.clone(),
                ));
                self.app.set_status("Session list is current");
            }
            RequestEvent::ModelsLoaded { service, models } => {
                self.complete_refresh_query();
                self.app.replace_models(service, models)?;
                self.app.set_status("Reviewed model availability updated");
            }
            RequestEvent::CredentialStatusLoaded(status) => {
                self.complete_refresh_query();
                self.app.set_credential_status(status);
                if let Some(outcome_unknown) = self.credential_reconciliation_unknown.take() {
                    self.app.set_status(if outcome_unknown {
                        "Credential state reloaded after an unknown outcome; the secret was cleared and the mutation was not retried"
                    } else {
                        "Credential state reloaded after the rejected mutation"
                    });
                }
            }
            RequestEvent::ContextLoaded(context) => {
                let percent = u64::from(context.estimated_input_tokens)
                    .saturating_mul(100)
                    .checked_div(u64::from(context.maximum_input_tokens))
                    .unwrap_or(0);
                let checkpoint = context.checkpoint_source_entry_high_water.map_or_else(
                    || "no checkpoint".to_owned(),
                    |high_water| {
                        format!(
                            "checkpoint through entry {high_water} · ~{} summary tokens",
                            context.checkpoint_estimated_summary_tokens.unwrap_or(0)
                        )
                    },
                );
                self.app.context_status_loaded(context.clone())?;
                self.app.set_status(format!(
                    "Context ~{} / {} tokens ({percent}%) · compacts at {} · reserves {} input and up to {} output · {checkpoint}",
                    context.estimated_input_tokens,
                    context.maximum_input_tokens,
                    context.compaction_threshold_tokens,
                    context
                        .maximum_input_tokens
                        .saturating_sub(context.compaction_threshold_tokens),
                    context.maximum_output_tokens,
                ));
            }
            RequestEvent::SessionLoaded(snapshot) => {
                let skill_warning = snapshot.skill_warnings.first().cloned();
                let additional_warnings = snapshot.skill_warnings.len().saturating_sub(1);
                self.install_session_snapshot(snapshot, subscription_events)?;
                let shared = self.app.selected_directory_is_shared();
                self.app.set_status(match (skill_warning, shared) {
                    (None, false) => "Session transcript and skills are current".to_owned(),
                    (None, true) => "Warning: another session uses this directory; filesystem effects may race".to_owned(),
                    (Some(warning), false) if additional_warnings == 0 => {
                        format!("Skill warning: {warning}")
                    }
                    (Some(warning), false) => {
                        format!("Skill warning: {warning} (+{additional_warnings} more)")
                    }
                    (Some(warning), true) => format!(
                        "Shared-directory race warning · skill warning: {warning} (+{additional_warnings} more)"
                    ),
                });
            }
            RequestEvent::SessionCreated {
                mutation_request_id,
                session,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.add_session(session.clone())?;
                self.requested_session = Some(session.id);
                send_command(commands, RequestCommand::LoadSession(session.id))?;
                self.app.set_status("Session created");
            }
            RequestEvent::SessionRenamed {
                mutation_request_id,
                session,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.rename_session_applied(session)?;
                self.app.set_status("Session renamed");
            }
            RequestEvent::InputAccepted {
                mutation_request_id,
                accepted,
            } => {
                let manual_compaction = matches!(
                    self.pending_command.as_ref(),
                    Some(RequestCommand::SubmitInput { text, .. })
                        if text == "/compact" || text.starts_with("/compact ")
                );
                self.finish_mutation(mutation_request_id)?;
                self.app.session_input_accepted(accepted.run)?;
                self.app.set_status(if manual_compaction {
                    "Manual compaction accepted; summary and continuation are server-owned"
                } else {
                    "Message accepted durably; run is server-owned"
                });
            }
            RequestEvent::LocalCommandAccepted {
                mutation_request_id,
                session_id,
                command_id,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.local_command_accepted(session_id, command_id)?;
                self.app.set_status("Local command accepted and running");
            }
            RequestEvent::CancellationResolved {
                mutation_request_id,
                result,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.cancellation_resolved(
                    result.run_id,
                    result.state,
                    result.cancellation_requested,
                )?;
                self.app.set_status(if result.cancellation_requested {
                    "Cancellation intent committed; waiting for controlled execution to stop"
                } else {
                    "Run was already terminal"
                });
            }
            RequestEvent::LocalCommandCancellationResolved {
                mutation_request_id,
                command_id,
                cancellation_requested,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.local_command_cancellation_resolved(command_id)?;
                self.app.set_status(if cancellation_requested {
                    "Cancellation requested; waiting for the local command tree to stop"
                } else {
                    "The local command was already terminal"
                });
            }
            RequestEvent::ToolUncertaintyAcknowledged {
                mutation_request_id,
                session_id,
                workspace,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.workspace_updated(session_id, workspace)?;
                self.app.set_status(
                    "Uncertain effect acknowledged and parked; no effect was retried or resolved",
                );
            }
            RequestEvent::CredentialUpdated {
                mutation_request_id,
                credential,
            } => {
                self.finish_credential_mutation(mutation_request_id)?;
                self.app.set_credential_status(credential);
                self.app.set_status(if credential.configured {
                    "OpenCode credential stored; validity is checked only by a deliberate provider request"
                } else {
                    "OpenCode credential removed"
                });
            }
            RequestEvent::CredentialMutationFailed {
                mutation_request_id,
                context,
                error,
                outcome_unknown,
            } => {
                self.finish_credential_mutation(mutation_request_id)?;
                self.app.mark_credential_status_unknown();
                self.credential_reconciliation_unknown = Some(outcome_unknown);
                send_command(commands, RequestCommand::LoadCredentialStatus)?;
                self.app.set_status(if outcome_unknown {
                    format!(
                        "{context} outcome is unknown: {error}; the secret was cleared, the mutation was not retried, and status is being reloaded"
                    )
                } else {
                    format!("{context} failed: {error}; credential status is being reloaded")
                });
            }
            RequestEvent::ServerStopAccepted {
                mutation_request_id,
                result,
            } => {
                self.finish_mutation(mutation_request_id)?;
                if result.current_server_stopping {
                    return Ok(true);
                }
                self.app
                    .set_status("The stop request belonged to an earlier server generation");
            }
            RequestEvent::QueryFailed {
                context,
                model_service,
                error,
            } => {
                if matches!(context, "session list" | "model list" | "credential status") {
                    self.complete_refresh_query();
                }
                if let Some(service) = model_service {
                    self.app.replace_models(service, Vec::new())?;
                }
                self.app.set_status(format!("{context} failed: {error}"));
            }
            RequestEvent::MutationFailed {
                mutation_request_id,
                context,
                error,
            } => {
                self.finish_mutation(mutation_request_id)?;
                self.app.set_status(format!("{context} failed: {error}"));
            }
            RequestEvent::MutationOutcomeUnknown {
                mutation_request_id,
                context,
                error,
            } => {
                self.require_pending_mutation(mutation_request_id)?;
                self.app.mark_pending_unknown();
                self.app.set_status(format!(
                    "{context} outcome is unknown: {error}; r retries the exact request, a abandons it"
                ));
            }
        }
        Ok(false)
    }

    fn handle_subscription_event(
        &mut self,
        event: SubscriptionEvent,
        commands: &mpsc::Sender<RequestCommand>,
        _subscription_events: &mpsc::Sender<SubscriptionEvent>,
    ) -> Result<(), TerminalApplicationError> {
        match event {
            SubscriptionEvent::Catalog(event) => self.app.apply_event(event)?,
            SubscriptionEvent::Session { generation, event }
                if generation == self.session_generation =>
            {
                self.app.apply_event(event)?;
                self.app.transcript_scroll = 0;
            }
            SubscriptionEvent::Session { .. } => {}
            SubscriptionEvent::CatalogConnectionLost => {
                self.app.clear_credential_interaction();
                self.app.set_status(
                    "Session catalog connection lost; reconnecting and clearing transient input",
                );
            }
            SubscriptionEvent::SessionConnectionLost { generation }
                if generation == self.session_generation =>
            {
                self.app.clear_transient_assistant();
                self.app.clear_credential_interaction();
                self.app.set_status(
                    "Session connection lost; transient output and credential input were discarded",
                );
            }
            SubscriptionEvent::SessionConnectionLost { .. } => {}
            SubscriptionEvent::CatalogSnapshotRequired => {
                send_command(commands, RequestCommand::LoadSessions)?;
                self.app
                    .set_status("Session catalog cursor expired; loading a new snapshot");
            }
            SubscriptionEvent::SessionSnapshotRequired {
                generation,
                session_id,
            } if generation == self.session_generation
                && self.requested_session == Some(session_id) =>
            {
                send_command(commands, RequestCommand::LoadSession(session_id))?;
                self.app
                    .set_status("Session cursor expired; loading a new transcript snapshot");
            }
            SubscriptionEvent::SessionSnapshotRequired { .. } => {}
            SubscriptionEvent::Failed { scope, error } => {
                if scope == "session subscription" {
                    self.app.clear_transient_assistant();
                }
                self.app.clear_credential_interaction();
                self.app.set_status(format!("{scope} stopped: {error}"));
            }
        }
        Ok(())
    }

    fn complete_refresh_query(&mut self) {
        self.refresh_remaining = self.refresh_remaining.saturating_sub(1);
    }

    fn start_mutation(
        &mut self,
        command: RequestCommand,
        operation: PendingOperation,
        commands: &mpsc::Sender<RequestCommand>,
    ) -> Result<(), TerminalApplicationError> {
        if self.pending_command.is_some() || self.pending_credential_mutation.is_some() {
            return Ok(());
        }
        let retry_command = command
            .clone_for_retry()
            .ok_or(TerminalApplicationError::State)?;
        send_command(commands, command)?;
        self.pending_command = Some(retry_command);
        self.app.mark_pending(operation);
        Ok(())
    }

    fn start_credential_mutation(
        &mut self,
        mutation_request_id: MutationRequestId,
        command: RequestCommand,
        commands: &mpsc::Sender<RequestCommand>,
    ) -> Result<(), TerminalApplicationError> {
        if self.pending_command.is_some() || self.pending_credential_mutation.is_some() {
            return Err(TerminalApplicationError::State);
        }
        if command.clone_for_retry().is_some()
            || command.mutation_request_id() != Some(mutation_request_id)
        {
            return Err(TerminalApplicationError::State);
        }
        send_command(commands, command)?;
        self.pending_credential_mutation = Some(mutation_request_id);
        self.app.mark_pending(PendingOperation::UpdateCredential);
        Ok(())
    }

    fn finish_mutation(
        &mut self,
        mutation_request_id: MutationRequestId,
    ) -> Result<(), TerminalApplicationError> {
        self.require_pending_mutation(mutation_request_id)?;
        self.pending_command = None;
        self.app.clear_pending();
        Ok(())
    }

    fn finish_credential_mutation(
        &mut self,
        mutation_request_id: MutationRequestId,
    ) -> Result<(), TerminalApplicationError> {
        if self.pending_credential_mutation != Some(mutation_request_id) {
            return Err(TerminalApplicationError::State);
        }
        self.pending_credential_mutation = None;
        self.app.clear_pending();
        Ok(())
    }

    fn require_pending_mutation(
        &self,
        mutation_request_id: MutationRequestId,
    ) -> Result<(), TerminalApplicationError> {
        if self
            .pending_command
            .as_ref()
            .and_then(RequestCommand::mutation_request_id)
            == Some(mutation_request_id)
        {
            Ok(())
        } else {
            Err(TerminalApplicationError::State)
        }
    }

    fn install_session_snapshot(
        &mut self,
        snapshot: SessionSnapshot,
        subscription_events: &mpsc::Sender<SubscriptionEvent>,
    ) -> Result<(), TerminalApplicationError> {
        if self.requested_session != Some(snapshot.session.id) {
            return Ok(());
        }
        let session_id = snapshot.session.id;
        let event_cursor = snapshot.event_cursor;
        self.app.open_session(
            snapshot.session,
            snapshot.workspace,
            snapshot.entries,
            snapshot.runs,
            snapshot.active_run_id,
            snapshot.active_command_id,
        )?;
        self.app
            .install_session_skills(session_id, snapshot.skills)?;
        self.session_generation = self.session_generation.wrapping_add(1);
        abort_task(&mut self.session_subscription);
        self.session_subscription = Some(spawn_session_subscription(
            session_id,
            event_cursor,
            self.session_generation,
            subscription_events.clone(),
        ));
        Ok(())
    }

    fn abort_background_tasks(&mut self) {
        abort_task(&mut self.catalog_subscription);
        abort_task(&mut self.session_subscription);
        self.request_worker.abort();
    }
}

async fn capture_pasted_image(
    value: &str,
) -> Option<Result<(morons_image::NormalizedImage, String), String>> {
    let path = pasted_image_path(value)?;
    if !path.is_file() {
        return None;
    }
    let display_name = path.file_name()?.to_str()?.to_owned();
    Some(
        tokio::task::spawn_blocking(move || {
            let metadata =
                fs::metadata(&path).map_err(|_| "Image path could not be read".to_owned())?;
            if metadata.len() == 0 || metadata.len() > morons_image::MAX_INPUT_IMAGE_BYTES as u64 {
                return Err("Image path exceeds the input byte limit".to_owned());
            }
            let bytes = fs::read(path).map_err(|_| "Image path could not be read".to_owned())?;
            morons_image::normalize_image(&bytes)
                .map(|image| (image, display_name))
                .map_err(|_| {
                    "Image path is unsupported, malformed, or exceeds image limits".to_owned()
                })
        })
        .await
        .unwrap_or_else(|_| Err("Image processing stopped unexpectedly".to_owned())),
    )
}

fn pasted_image_path(value: &str) -> Option<PathBuf> {
    if value.contains(['\n', '\r', '\0']) {
        return None;
    }
    let mut value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value = &value[1..value.len() - 1];
    }
    let decoded;
    if let Some(url_path) = value.strip_prefix("file://") {
        decoded = percent_decode_path(url_path)?;
        value = &decoded;
    }
    let path = PathBuf::from(value);
    let path = if path.is_file() {
        path
    } else {
        #[cfg(not(windows))]
        {
            PathBuf::from(unescape_drag_path(value))
        }
        #[cfg(windows)]
        {
            path
        }
    };
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())?
        .to_ascii_lowercase();
    matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp").then_some(path)
}

#[cfg(not(windows))]
fn unescape_drag_path(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(next) = characters.next() {
                output.push(next);
            }
        } else {
            output.push(character);
        }
    }
    output
}

fn percent_decode_path(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = decode_hex(input.next()?)?;
            let low = decode_hex(input.next()?)?;
            bytes.push((high << 4) | low);
        } else {
            bytes.push(byte);
        }
    }
    let decoded = String::from_utf8(bytes).ok()?;
    #[cfg(windows)]
    let decoded = decoded
        .strip_prefix('/')
        .filter(|value| value.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&decoded)
        .to_owned();
    Some(decoded)
}

fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn enqueue_initial_queries(
    commands: &mpsc::Sender<RequestCommand>,
) -> Result<(), TerminalApplicationError> {
    for command in [
        RequestCommand::LoadSessions,
        RequestCommand::LoadCredentialStatus,
        RequestCommand::LoadModels(OpenCodeService::Zen),
        RequestCommand::LoadModels(OpenCodeService::Go),
    ] {
        send_command(commands, command)?;
    }
    Ok(())
}

fn send_command(
    commands: &mpsc::Sender<RequestCommand>,
    command: RequestCommand,
) -> Result<(), TerminalApplicationError> {
    commands
        .try_send(command)
        .map_err(|_| TerminalApplicationError::RequestWorkerStopped)
}

fn abort_task(task: &mut Option<JoinHandle<()>>) {
    if let Some(task) = task.take() {
        task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_application_errors_do_not_render_nested_input() {
        let error = TerminalApplicationError::Terminal(io::Error::other(
            "sensitive path\u{1b}]52;c;clipboard",
        ));
        assert_eq!(error.to_string(), "terminal application failed");
        assert_eq!(format!("{error:?}"), "TerminalApplicationError::Terminal");
    }

    #[tokio::test]
    async fn pasted_and_file_url_image_paths_are_captured_immediately() {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("randomness should be available");
        let root = std::env::temp_dir().join(format!(
            "morons-pasted-image-{}-{}",
            std::process::id(),
            nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        fs::create_dir(&root).expect("test directory should be created");
        let path = root.join("picture with spaces.png");
        let image = morons_image::normalize_rgba(2, 1, vec![0x99; 8])
            .expect("fixture image should normalize");
        fs::write(&path, &image.bytes).expect("fixture image should be written");
        let captured = capture_pasted_image(&path.to_string_lossy())
            .await
            .expect("path should be recognized")
            .expect("path should normalize");
        assert_eq!(captured.1, "picture with spaces.png");
        let encoded = path.to_string_lossy().replace(' ', "%20");
        let captured = capture_pasted_image(&format!("file://{encoded}"))
            .await
            .expect("file URL should be recognized")
            .expect("file URL should normalize");
        assert_eq!((captured.0.width, captured.0.height), (2, 1));
        fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[tokio::test]
    async fn unknown_credential_outcome_is_not_retried_and_reloads_status() {
        let request_worker = tokio::spawn(async {});
        let mut runtime = RuntimeState::new("test-server".to_owned(), request_worker);
        let mutation_request_id = MutationRequestId::from_bytes([0x44; 16]);
        runtime.pending_credential_mutation = Some(mutation_request_id);
        runtime.app.mark_pending(PendingOperation::UpdateCredential);
        let (commands, mut command_receiver) = mpsc::channel(2);
        let (subscription_events, _subscription_receiver) = mpsc::channel(2);

        let should_exit = runtime
            .handle_request_event(
                RequestEvent::CredentialMutationFailed {
                    mutation_request_id,
                    context: "credential configuration",
                    error: "connection ended".to_owned(),
                    outcome_unknown: true,
                },
                &commands,
                &subscription_events,
            )
            .await
            .expect("unknown outcome should reconcile");

        assert!(!should_exit);
        assert!(runtime.pending_credential_mutation.is_none());
        assert!(runtime.pending_command.is_none());
        assert!(runtime.app.pending.is_none());
        assert!(runtime.app.credential.is_none());
        assert_eq!(runtime.credential_reconciliation_unknown, Some(true));
        assert!(matches!(
            command_receiver.try_recv(),
            Ok(RequestCommand::LoadCredentialStatus)
        ));
    }
}
