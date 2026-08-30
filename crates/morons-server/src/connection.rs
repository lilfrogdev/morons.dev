use std::{error::Error, fmt};

use morons_protocol::{
    ClientMessage, FrameError, PROTOCOL_VERSION, ServerMessage, read_client_message,
    write_server_message,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::application::ServerApplication;

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
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(error) => write!(formatter, "application frame failed: {error}"),
            Self::UnexpectedClientMessage => {
                formatter.write_str("client message is invalid in the current protocol state")
            }
        }
    }
}

impl Error for ConnectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            Self::UnexpectedClientMessage => None,
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

        let response = match application.execute_for_local_owner(request).await {
            Ok(response) => ServerMessage::response(request_id, response),
            Err(error) => ServerMessage::request_failed(request_id, error),
        };
        write_server_message(connection, &response).await?;
    }
}
