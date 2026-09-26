use morons_protocol as protocol;
use tokio::sync::watch;

use super::{ApplicationOutcome, ServerApplication, conversions::*};
use crate::persistence::steering as storage;

pub(crate) struct SteeringSubscription {
    pub cursor: protocol::SteeringCursor,
    pub notifications: watch::Receiver<u64>,
}

fn storage_cursor(cursor: protocol::SteeringCursor) -> storage::SteeringCursor {
    storage::SteeringCursor {
        session_id: to_persistence_session_id(cursor.session_id),
        sequence: cursor.sequence,
    }
}

fn protocol_cursor(cursor: storage::SteeringCursor) -> protocol::SteeringCursor {
    protocol::SteeringCursor {
        session_id: protocol::SessionId::from_bytes(*cursor.session_id.as_bytes()),
        sequence: cursor.sequence,
    }
}

impl ServerApplication {
    pub(crate) async fn read_steering_events(
        &self,
        cursor: protocol::SteeringCursor,
        limit: u16,
    ) -> Result<protocol::SteeringPage, protocol::ApplicationError> {
        let page = self
            .sessions
            .steering_replay(storage_cursor(cursor), limit)
            .await
            .map_err(to_application_error)?;
        Ok(protocol::SteeringPage {
            high_water: protocol_cursor(page.high_water),
            notices: page
                .notices
                .into_iter()
                .map(|notice| protocol::SteeringNotice {
                    cursor: protocol_cursor(notice.cursor),
                    revision: notice.revision,
                })
                .collect(),
        })
    }

    pub(super) async fn execute_steering(
        &self,
        request: protocol::ApplicationRequest,
    ) -> Result<ApplicationOutcome, protocol::ApplicationError> {
        use protocol::{ApplicationRequest as Request, ApplicationResponse as Response};
        let response = match request {
            Request::MutateSteering { mutation } => {
                let guard = self.lifecycle_mutations.lock().await;
                let change = match mutation.change {
                    protocol::SteeringChange::Enqueue { run_id, text } => {
                        storage::SteeringChange::Enqueue {
                            run_id: to_persistence_run_id(run_id),
                            text,
                        }
                    }
                    protocol::SteeringChange::Edit {
                        item_id,
                        revision,
                        text,
                    } => storage::SteeringChange::Edit {
                        item_id,
                        revision,
                        text,
                    },
                    protocol::SteeringChange::Remove { item_id, revision } => {
                        storage::SteeringChange::Remove { item_id, revision }
                    }
                    protocol::SteeringChange::Pause => storage::SteeringChange::Pause,
                    protocol::SteeringChange::Resume { run_id } => {
                        storage::SteeringChange::Resume {
                            run_id: to_persistence_run_id(run_id),
                        }
                    }
                };
                let mutation = storage::SteeringMutation {
                    request_id: to_persistence_mutation_id(mutation.request_id),
                    session_id: to_persistence_session_id(mutation.session_id),
                    expected_revision: mutation.expected_revision,
                    change,
                };
                let receipt = match self
                    .sessions
                    .lookup_steering_mutation(mutation.clone())
                    .await
                    .map_err(to_application_error)?
                {
                    Some(receipt) => receipt,
                    None => {
                        if matches!(
                            mutation.change,
                            storage::SteeringChange::Enqueue { .. }
                                | storage::SteeringChange::Resume { .. }
                        ) && (self.stopping.load(std::sync::atomic::Ordering::Acquire)
                            || self.run_supervisor.is_stopping())
                        {
                            return Err(protocol::ApplicationError::ServiceUnavailable);
                        }
                        drop(guard);
                        let skills = match &mutation.change {
                            storage::SteeringChange::Enqueue { text, .. }
                            | storage::SteeringChange::Edit { text, .. } => {
                                storage::validate_text(text).map_err(to_application_error)?;
                                let directory = self
                                    .sessions
                                    .get_session(mutation.session_id)
                                    .await
                                    .map_err(to_application_error)?
                                    .ok_or(protocol::ApplicationError::SessionNotFound)?
                                    .working_directory
                                    .ok_or(
                                        protocol::ApplicationError::WorkingDirectoryUnavailable,
                                    )?;
                                let service = std::sync::Arc::clone(&self.skills);
                                let text = text.clone();
                                Some(
                                    tokio::task::spawn_blocking(move || {
                                        service.context(std::path::Path::new(&directory), &text)
                                    })
                                    .await
                                    .map_err(|_| protocol::ApplicationError::ServiceUnavailable)?,
                                )
                            }
                            _ => None,
                        };
                        let _guard = self.lifecycle_mutations.lock().await;
                        if let Some(receipt) = self
                            .sessions
                            .lookup_steering_mutation(mutation.clone())
                            .await
                            .map_err(to_application_error)?
                        {
                            receipt
                        } else {
                            if matches!(
                                mutation.change,
                                storage::SteeringChange::Enqueue { .. }
                                    | storage::SteeringChange::Resume { .. }
                            ) && (self.stopping.load(std::sync::atomic::Ordering::Acquire)
                                || self.run_supervisor.is_stopping())
                            {
                                return Err(protocol::ApplicationError::ServiceUnavailable);
                            }
                            self.sessions
                                .mutate_steering_with_skills(mutation, skills)
                                .await
                                .map_err(to_application_error)?
                        }
                    }
                };
                Response::SteeringMutated {
                    receipt: protocol::SteeringReceipt {
                        sequence: receipt.sequence,
                        queue_revision: receipt.queue_revision,
                        item_id: receipt.item_id,
                        item_revision: receipt.item_revision,
                    },
                }
            }
            Request::GetSteering { session_id } => {
                let snapshot = self
                    .sessions
                    .steering_snapshot(to_persistence_session_id(session_id))
                    .await
                    .map_err(to_application_error)?;
                Response::SteeringFound {
                    snapshot: protocol::SteeringSnapshot {
                        cursor: protocol_cursor(snapshot.cursor),
                        revision: snapshot.revision,
                        target_run_id: snapshot
                            .target_run_id
                            .map(|id| protocol::RunId::from_bytes(*id.as_bytes())),
                        paused: snapshot.paused,
                        items: snapshot
                            .items
                            .into_iter()
                            .map(|item| protocol::SteeringItem {
                                id: item.id,
                                revision: item.revision,
                                enqueue_sequence: item.enqueue_sequence,
                                text: item.text,
                            })
                            .collect(),
                    },
                }
            }
            Request::ReplaySteering { cursor, limit } => Response::SteeringReplayed {
                page: self.read_steering_events(cursor, limit).await?,
            },
            Request::SubscribeSteering { cursor } => {
                let notifications = self.sessions.subscribe_event_notifications();
                self.read_steering_events(cursor, 1).await?;
                return Ok(ApplicationOutcome::SteeringSubscription(
                    SteeringSubscription {
                        cursor,
                        notifications,
                    },
                ));
            }
            _ => unreachable!("only steering requests are routed here"),
        };
        Ok(ApplicationOutcome::Response(response))
    }
}
