use std::time::Duration;

use morons_protocol::{
    ApplicationEvent, FrameError, SessionCatalogEventCursor, SessionEventCursor, SessionId,
};
use tokio::{sync::mpsc, task::JoinHandle, time};

use crate::{ApplicationClient, ApplicationClientError, connect_or_start};

const MAX_RECONNECT_ATTEMPTS: usize = 5;
const RECONNECT_DELAY: Duration = Duration::from_millis(200);

pub(super) enum SubscriptionEvent {
    Catalog(ApplicationEvent),
    Session {
        generation: u64,
        event: ApplicationEvent,
    },
    CatalogConnectionLost,
    SessionConnectionLost {
        generation: u64,
    },
    CatalogSnapshotRequired,
    SessionSnapshotRequired {
        generation: u64,
        session_id: SessionId,
    },
    Failed {
        scope: &'static str,
        error: String,
    },
}

pub(super) fn spawn_catalog_subscription(
    cursor: SessionCatalogEventCursor,
    events: mpsc::Sender<SubscriptionEvent>,
) -> JoinHandle<()> {
    tokio::spawn(run_catalog_subscription(cursor, events))
}

pub(super) fn spawn_session_subscription(
    session_id: SessionId,
    cursor: SessionEventCursor,
    generation: u64,
    events: mpsc::Sender<SubscriptionEvent>,
) -> JoinHandle<()> {
    tokio::spawn(run_session_subscription(
        session_id, cursor, generation, events,
    ))
}

async fn run_catalog_subscription(
    mut cursor: SessionCatalogEventCursor,
    events: mpsc::Sender<SubscriptionEvent>,
) {
    let mut failures = 0_usize;
    loop {
        let connected = match connect_or_start().await {
            Ok(connected) => connected,
            Err(error) => {
                if failures == 0 {
                    let _ = events.send(SubscriptionEvent::CatalogConnectionLost).await;
                }
                failures += 1;
                if failures >= MAX_RECONNECT_ATTEMPTS {
                    let _ = events
                        .send(SubscriptionEvent::Failed {
                            scope: "session catalog subscription",
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
                time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        let client = ApplicationClient::from_negotiated_connection(connected.into_connection());
        let mut subscription = match client.subscribe_to_session_catalog(cursor).await {
            Ok(subscription) => subscription,
            Err(ApplicationClientError::Application(_)) => {
                let _ = events
                    .send(SubscriptionEvent::CatalogSnapshotRequired)
                    .await;
                return;
            }
            Err(error) if is_reconnectable(&error) => {
                if failures == 0 {
                    let _ = events.send(SubscriptionEvent::CatalogConnectionLost).await;
                }
                failures += 1;
                if failures >= MAX_RECONNECT_ATTEMPTS {
                    let _ = events
                        .send(SubscriptionEvent::Failed {
                            scope: "session catalog subscription",
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
                time::sleep(RECONNECT_DELAY).await;
                continue;
            }
            Err(error) => {
                let _ = events
                    .send(SubscriptionEvent::Failed {
                        scope: "session catalog subscription",
                        error: error.to_string(),
                    })
                    .await;
                return;
            }
        };
        failures = 0;
        loop {
            match subscription.next_event().await {
                Ok(event) => {
                    cursor = subscription.cursor();
                    failures = 0;
                    if events
                        .send(SubscriptionEvent::Catalog(event))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(ApplicationClientError::Application(_)) => {
                    let _ = events
                        .send(SubscriptionEvent::CatalogSnapshotRequired)
                        .await;
                    return;
                }
                Err(error) if is_reconnectable(&error) => {
                    let _ = events.send(SubscriptionEvent::CatalogConnectionLost).await;
                    break;
                }
                Err(error) => {
                    let _ = events
                        .send(SubscriptionEvent::Failed {
                            scope: "session catalog subscription",
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
            }
        }
        failures += 1;
        if failures >= MAX_RECONNECT_ATTEMPTS {
            let _ = events
                .send(SubscriptionEvent::Failed {
                    scope: "session catalog subscription",
                    error: "subscription reconnect limit reached".to_owned(),
                })
                .await;
            return;
        }
        time::sleep(RECONNECT_DELAY).await;
    }
}

async fn run_session_subscription(
    session_id: SessionId,
    mut cursor: SessionEventCursor,
    generation: u64,
    events: mpsc::Sender<SubscriptionEvent>,
) {
    let mut failures = 0_usize;
    loop {
        let connected = match connect_or_start().await {
            Ok(connected) => connected,
            Err(error) => {
                if failures == 0 {
                    let _ = events
                        .send(SubscriptionEvent::SessionConnectionLost { generation })
                        .await;
                }
                failures += 1;
                if failures >= MAX_RECONNECT_ATTEMPTS {
                    let _ = events
                        .send(SubscriptionEvent::Failed {
                            scope: "session subscription",
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
                time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        let client = ApplicationClient::from_negotiated_connection(connected.into_connection());
        let mut subscription = match client.subscribe_to_session(session_id, cursor).await {
            Ok(subscription) => subscription,
            Err(ApplicationClientError::Application(_)) => {
                let _ = events
                    .send(SubscriptionEvent::SessionSnapshotRequired {
                        generation,
                        session_id,
                    })
                    .await;
                return;
            }
            Err(error) if is_reconnectable(&error) => {
                if failures == 0 {
                    let _ = events
                        .send(SubscriptionEvent::SessionConnectionLost { generation })
                        .await;
                }
                failures += 1;
                if failures >= MAX_RECONNECT_ATTEMPTS {
                    let _ = events
                        .send(SubscriptionEvent::Failed {
                            scope: "session subscription",
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
                time::sleep(RECONNECT_DELAY).await;
                continue;
            }
            Err(error) => {
                let _ = events
                    .send(SubscriptionEvent::Failed {
                        scope: "session subscription",
                        error: error.to_string(),
                    })
                    .await;
                return;
            }
        };
        failures = 0;
        loop {
            match subscription.next_event().await {
                Ok(event) => {
                    cursor = subscription.cursor();
                    failures = 0;
                    if events
                        .send(SubscriptionEvent::Session { generation, event })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Err(ApplicationClientError::Application(_)) => {
                    let _ = events
                        .send(SubscriptionEvent::SessionSnapshotRequired {
                            generation,
                            session_id,
                        })
                        .await;
                    return;
                }
                Err(error) if is_reconnectable(&error) => {
                    let _ = events
                        .send(SubscriptionEvent::SessionConnectionLost { generation })
                        .await;
                    break;
                }
                Err(error) => {
                    let _ = events
                        .send(SubscriptionEvent::Failed {
                            scope: "session subscription",
                            error: error.to_string(),
                        })
                        .await;
                    return;
                }
            }
        }
        failures += 1;
        if failures >= MAX_RECONNECT_ATTEMPTS {
            let _ = events
                .send(SubscriptionEvent::Failed {
                    scope: "session subscription",
                    error: "subscription reconnect limit reached".to_owned(),
                })
                .await;
            return;
        }
        time::sleep(RECONNECT_DELAY).await;
    }
}

fn is_reconnectable(error: &ApplicationClientError) -> bool {
    matches!(
        error,
        ApplicationClientError::ServerDisconnected
            | ApplicationClientError::ConnectionUnusable
            | ApplicationClientError::RequestIdentifierExhausted
            | ApplicationClientError::Frame(FrameError::Io(_))
    )
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn only_transport_loss_is_reconnected() {
        assert!(is_reconnectable(
            &ApplicationClientError::ServerDisconnected
        ));
        assert!(is_reconnectable(&ApplicationClientError::Frame(
            FrameError::Io(io::Error::from(io::ErrorKind::ConnectionReset))
        )));
        assert!(!is_reconnectable(
            &ApplicationClientError::UnexpectedServerMessage
        ));
        assert!(!is_reconnectable(
            &ApplicationClientError::EventScopeMismatch
        ));
    }
}
