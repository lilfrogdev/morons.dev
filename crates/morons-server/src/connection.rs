use std::{error::Error, fmt, time::Duration};

use morons_protocol::{
    ApplicationResponse, ClientMessage, FrameError, PROTOCOL_VERSION, ServerMessage,
    read_client_message, write_server_message,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time,
};

use crate::application::{
    ApplicationOutcome, ServerApplication, SessionCatalogSubscription, SessionSubscription,
};

#[cfg(not(test))]
const SUBSCRIPTION_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(test)]
const SUBSCRIPTION_WRITE_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeOutcome {
    Accepted,
    Rejected,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ConnectionError {
    Frame(FrameError),
    UnexpectedClientMessage,
    SubscriptionWriteTimedOut,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "application frame failed: {error}"),
            Self::UnexpectedClientMessage => {
                formatter.write_str("client message is invalid in the current protocol state")
            }
            Self::SubscriptionWriteTimedOut => {
                formatter.write_str("application subscriber stopped accepting events")
            }
        }
    }
}

impl Error for ConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::UnexpectedClientMessage | Self::SubscriptionWriteTimedOut => None,
        }
    }
}

impl From<FrameError> for ConnectionError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

pub async fn handle_handshake<S>(
    connection: &mut S,
    server_version: &str,
) -> Result<HandshakeOutcome, ConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(message) = read_client_message(connection).await? else {
        return Ok(HandshakeOutcome::Rejected);
    };

    let response = match message {
        ClientMessage::Hello {
            protocol_version, ..
        } if protocol_version == PROTOCOL_VERSION => {
            write_server_message(connection, &ServerMessage::hello(server_version)).await?;
            return Ok(HandshakeOutcome::Accepted);
        }
        ClientMessage::Hello {
            protocol_version, ..
        } => ServerMessage::protocol_version_mismatch(protocol_version),
        ClientMessage::Request { .. } => return Err(ConnectionError::UnexpectedClientMessage),
    };

    write_server_message(connection, &response).await?;
    Ok(HandshakeOutcome::Rejected)
}

pub async fn handle_local_owner_requests<S>(
    connection: &mut S,
    application: &ServerApplication,
) -> Result<(), ConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let Some(message) = read_client_message(connection).await? else {
            return Ok(());
        };
        let ClientMessage::Request {
            request_id,
            request,
        } = message
        else {
            return Err(ConnectionError::UnexpectedClientMessage);
        };

        match application.execute_for_local_owner(request).await {
            Ok(ApplicationOutcome::Response(response)) => {
                write_server_message(connection, &ServerMessage::response(request_id, response))
                    .await?;
            }
            Ok(ApplicationOutcome::SessionCatalogSubscription(subscription)) => {
                write_server_message(
                    connection,
                    &ServerMessage::response(
                        request_id,
                        ApplicationResponse::SessionCatalogSubscriptionStarted {
                            cursor: subscription.protocol_cursor(),
                        },
                    ),
                )
                .await?;
                return stream_session_catalog_events(connection, application, subscription).await;
            }
            Ok(ApplicationOutcome::SessionSubscription(subscription)) => {
                write_server_message(
                    connection,
                    &ServerMessage::response(
                        request_id,
                        ApplicationResponse::SessionSubscriptionStarted {
                            session_id: morons_protocol::SessionId::from_bytes(
                                *subscription.session_id.as_bytes(),
                            ),
                            cursor: subscription.protocol_cursor(),
                        },
                    ),
                )
                .await?;
                return stream_session_events(connection, application, subscription).await;
            }
            Err(error) => {
                write_server_message(
                    connection,
                    &ServerMessage::request_failed(request_id, error),
                )
                .await?;
            }
        }
    }
}

async fn stream_session_catalog_events<S>(
    connection: &mut S,
    application: &ServerApplication,
    mut subscription: SessionCatalogSubscription,
) -> Result<(), ConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(connection);
    let client_message = read_client_message(&mut reader);
    tokio::pin!(client_message);

    loop {
        let observed_notification = *subscription.notifications.borrow_and_update();
        let events = match application
            .read_session_catalog_events(subscription.cursor)
            .await
        {
            Ok(events) => events,
            Err(error) => {
                write_subscription_message(&mut writer, &ServerMessage::subscription_ended(error))
                    .await?;
                return Ok(());
            }
        };
        if !events.is_empty() {
            for event in events {
                write_subscription_message(&mut writer, &ServerMessage::event(event.event)).await?;
                subscription.advance(event.cursor);
            }
            continue;
        }

        if *subscription.notifications.borrow() != observed_notification {
            continue;
        }

        tokio::select! {
            incoming = &mut client_message => {
                match incoming? {
                    None => return Ok(()),
                    Some(_) => return Err(ConnectionError::UnexpectedClientMessage),
                }
            }
            changed = subscription.notifications.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

async fn stream_session_events<S>(
    connection: &mut S,
    application: &ServerApplication,
    mut subscription: SessionSubscription,
) -> Result<(), ConnectionError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (mut reader, mut writer) = tokio::io::split(connection);
    let client_message = read_client_message(&mut reader);
    tokio::pin!(client_message);

    loop {
        let observed_notification = *subscription.notifications.borrow_and_update();
        let events = match application
            .read_session_events(subscription.session_id, subscription.cursor)
            .await
        {
            Ok(events) => events,
            Err(error) => {
                write_subscription_message(&mut writer, &ServerMessage::subscription_ended(error))
                    .await?;
                return Ok(());
            }
        };
        if !events.is_empty() {
            for event in events {
                subscription.advance(&event);
                write_subscription_message(&mut writer, &ServerMessage::event(event.event)).await?;
            }
            continue;
        }
        if *subscription.notifications.borrow() != observed_notification {
            continue;
        }

        tokio::select! {
            biased;
            incoming = &mut client_message => {
                match incoming? {
                    None => return Ok(()),
                    Some(_) => return Err(ConnectionError::UnexpectedClientMessage),
                }
            }
            changed = subscription.notifications.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
            }
            delta = subscription.assistant_deltas.recv() => {
                match delta {
                    Ok(delta) if subscription.accepts_delta(&delta) => {
                        let event = ServerApplication::assistant_delta_event(delta);
                        write_subscription_message(
                            &mut writer,
                            &ServerMessage::event(event),
                        )
                        .await?;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

async fn write_subscription_message<W>(
    writer: &mut W,
    message: &ServerMessage,
) -> Result<(), ConnectionError>
where
    W: AsyncWrite + Unpin,
{
    match time::timeout(
        SUBSCRIPTION_WRITE_TIMEOUT,
        write_server_message(writer, message),
    )
    .await
    {
        Ok(result) => result.map_err(ConnectionError::from),
        Err(_) => Err(ConnectionError::SubscriptionWriteTimedOut),
    }
}
