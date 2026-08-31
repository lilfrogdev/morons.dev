mod companion;

use std::{error::Error, fmt, io, path::PathBuf, process::Child, time::Duration};

use interprocess::local_socket::tokio::Stream;
use morons_protocol::{
    AuthenticationError, ClientEndpoint, ClientEndpointDiscovery, ControlError, authenticate_client,
};
use tokio::time::{self, Instant};

use self::companion::{discover_companion_executable, reap_exited_child, spawn_companion};
use crate::{HandshakeError, perform_handshake};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const DISCOVERY_RETRY_DELAY: Duration = Duration::from_millis(50);
const INCOMPLETE_CONTROL_GRACE: Duration = Duration::from_secs(2);

pub struct ConnectedServer {
    connection: Stream,
    server_version: String,
    launched_companion: bool,
}

impl ConnectedServer {
    #[must_use]
    pub fn into_connection(self) -> Stream {
        self.connection
    }

    #[must_use]
    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    #[must_use]
    pub const fn launched_companion(&self) -> bool {
        self.launched_companion
    }
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ConnectOrStartError {
    Control(ControlError),
    CompanionIo(io::Error),
    CompanionInvalid { reason: &'static str },
    Connect(io::Error),
    PeerAuthorization(io::Error),
    Authentication(AuthenticationError),
    AuthenticationTimedOut,
    Handshake(HandshakeError),
    HandshakeTimedOut,
    StartupTimedOut,
}

impl fmt::Display for ConnectOrStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Control(error) => write!(formatter, "local server discovery failed: {error}"),
            Self::CompanionIo(error) => {
                write!(formatter, "server companion launch failed: {error}")
            }
            Self::CompanionInvalid { reason } => {
                write!(formatter, "server companion is invalid: {reason}")
            }
            Self::Connect(error) => write!(formatter, "local server connection failed: {error}"),
            Self::PeerAuthorization(error) => {
                write!(formatter, "connected server authorization failed: {error}")
            }
            Self::Authentication(error) => {
                write!(formatter, "local server authentication failed: {error}")
            }
            Self::AuthenticationTimedOut => {
                formatter.write_str("local server authentication timed out")
            }
            Self::Handshake(error) => write!(formatter, "server handshake failed: {error}"),
            Self::HandshakeTimedOut => formatter.write_str("server handshake timed out"),
            Self::StartupTimedOut => formatter.write_str("local server startup timed out"),
        }
    }
}

impl Error for ConnectOrStartError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Control(error) => Some(error),
            Self::CompanionIo(error) | Self::Connect(error) | Self::PeerAuthorization(error) => {
                Some(error)
            }
            Self::Authentication(error) => Some(error),
            Self::Handshake(error) => Some(error),
            Self::CompanionInvalid { .. }
            | Self::AuthenticationTimedOut
            | Self::HandshakeTimedOut
            | Self::StartupTimedOut => None,
        }
    }
}

impl From<ControlError> for ConnectOrStartError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

pub async fn connect_or_start() -> Result<ConnectedServer, ConnectOrStartError> {
    connect_or_start_until(None, Instant::now() + STARTUP_TIMEOUT).await
}

async fn connect_or_start_until(
    mut companion: Option<PathBuf>,
    deadline: Instant,
) -> Result<ConnectedServer, ConnectOrStartError> {
    let mut launched_companion = false;
    let mut incomplete_control_since = None;
    let mut child: Option<Child> = None;
    loop {
        if Instant::now() >= deadline {
            return Err(ConnectOrStartError::StartupTimedOut);
        }

        let mut startup_allowed = false;
        match ClientEndpoint::discover()? {
            ClientEndpointDiscovery::Absent => {
                incomplete_control_since = None;
                startup_allowed = true;
            }
            ClientEndpointDiscovery::Incomplete => {
                let since = incomplete_control_since.get_or_insert_with(Instant::now);
                if Instant::now().duration_since(*since) >= INCOMPLETE_CONTROL_GRACE {
                    return Err(ConnectOrStartError::Control(ControlError::InvalidState {
                        reason: "local control initialization remained incomplete",
                    }));
                }
            }
            ClientEndpointDiscovery::Starting => incomplete_control_since = None,
            ClientEndpointDiscovery::Registered(endpoint) => {
                incomplete_control_since = None;
                if let Some(connection) =
                    connect_registered_server(endpoint, deadline, launched_companion).await?
                {
                    reap_exited_child(&mut child)?;
                    return Ok(connection);
                }
                startup_allowed = true;
            }
        }

        if startup_allowed && !launched_companion {
            let path = match companion.as_ref() {
                Some(path) => path,
                None => companion.insert(discover_companion_executable()?),
            };
            child = Some(spawn_companion(path)?);
            launched_companion = true;
        }
        reap_exited_child(&mut child)?;
        time::sleep_until((Instant::now() + DISCOVERY_RETRY_DELAY).min(deadline)).await;
    }
}

async fn connect_registered_server(
    endpoint: ClientEndpoint,
    deadline: Instant,
    launched_companion: bool,
) -> Result<Option<ConnectedServer>, ConnectOrStartError> {
    let connect_deadline = (Instant::now() + CONNECT_ATTEMPT_TIMEOUT).min(deadline);
    let mut connection = match time::timeout_at(connect_deadline, endpoint.connect()).await {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) if server_is_unavailable(&error) => return Ok(None),
        Ok(Err(error)) => return Err(ConnectOrStartError::Connect(error)),
        Err(_) => return Ok(None),
    };

    endpoint
        .verify_connected_server(&connection)
        .map_err(ConnectOrStartError::PeerAuthorization)?;
    match time::timeout_at(
        (Instant::now() + AUTHENTICATION_TIMEOUT).min(deadline),
        authenticate_client(
            &mut connection,
            endpoint.authentication_key(),
            endpoint.host_epoch(),
        ),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(ConnectOrStartError::Authentication(error)),
        Err(_) => return Err(ConnectOrStartError::AuthenticationTimedOut),
    }
    let server_version = match time::timeout_at(
        (Instant::now() + HANDSHAKE_TIMEOUT).min(deadline),
        perform_handshake(&mut connection, env!("CARGO_PKG_VERSION")),
    )
    .await
    {
        Ok(Ok(version)) => version,
        Ok(Err(error)) => return Err(ConnectOrStartError::Handshake(error)),
        Err(_) => return Err(ConnectOrStartError::HandshakeTimedOut),
    };
    Ok(Some(ConnectedServer {
        connection,
        server_version,
        launched_companion,
    }))
}

fn server_is_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::AddrNotAvailable
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_expected_connection_failures_are_startable() {
        assert!(server_is_unavailable(&io::Error::from(
            io::ErrorKind::ConnectionRefused
        )));
        assert!(!server_is_unavailable(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!server_is_unavailable(&io::Error::from(
            io::ErrorKind::InvalidData
        )));
    }
}
