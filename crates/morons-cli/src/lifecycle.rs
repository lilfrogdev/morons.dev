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
const STARTING_FEEDBACK: &str =
    "Waiting for local server initialization... Press Ctrl+C to stop waiting.";

#[derive(Default)]
struct StartupInitializationProgress {
    feedback_reported: bool,
    latest_discovery_was_starting: bool,
}

impl StartupInitializationProgress {
    fn observe(&mut self, discovery_is_starting: bool) -> (bool, bool) {
        let left_starting = self.latest_discovery_was_starting && !discovery_is_starting;
        self.latest_discovery_was_starting = discovery_is_starting;
        let report_starting =
            discovery_is_starting && !std::mem::replace(&mut self.feedback_reported, true);
        (report_starting, left_starting)
    }
}

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
    DebugRequiresStoppedServer,
    CompanionExited,
    StartupTimedOut,
}

impl ConnectOrStartError {
    const fn safe_description(&self) -> &'static str {
        match self {
            Self::Control(ControlError::SocketPathTooLong) => {
                "local Unix socket path is too long; use a shorter absolute HOME for a separate Morons profile; existing state is not moved"
            }
            Self::Control(_) => {
                "local control state could not be validated; automatic replacement was refused"
            }
            Self::CompanionIo(_) => {
                "packaged server companion could not be loaded or launched; reinstall matching Morons binaries together"
            }
            Self::CompanionInvalid { .. } => {
                "packaged server companion failed integrity validation; reinstall matching Morons binaries together"
            }
            Self::Connect(_) => "registered local server could not be reached safely",
            Self::PeerAuthorization(_) => {
                "registered local server failed operating-system peer authorization; automatic replacement was refused"
            }
            Self::Authentication(_) => {
                "registered local server failed mutual authentication; automatic replacement was refused"
            }
            Self::AuthenticationTimedOut => {
                "registered local server mutual authentication timed out"
            }
            Self::Handshake(HandshakeError::ProtocolVersionMismatch { .. }) => {
                "client and running server use incompatible protocol versions; stop the server with its matching client before upgrading"
            }
            Self::Handshake(_) => "registered local server failed protocol negotiation",
            Self::HandshakeTimedOut => "registered local server protocol negotiation timed out",
            Self::DebugRequiresStoppedServer => {
                "debug startup requires a stopped server; connect with morons, stop with Ctrl+S when work is idle, then run morons --debug; use morons to reconnect to an existing debug server"
            }
            Self::CompanionExited => {
                "server companion exited before becoming available; run the matching morons-server directly to diagnose startup; do not delete or downgrade existing state"
            }
            Self::StartupTimedOut => {
                "server companion did not become available before the startup timeout"
            }
        }
    }
}

impl fmt::Debug for ConnectOrStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Control(_) => "ConnectOrStartError::Control",
            Self::CompanionIo(_) => "ConnectOrStartError::CompanionIo",
            Self::CompanionInvalid { .. } => "ConnectOrStartError::CompanionInvalid",
            Self::Connect(_) => "ConnectOrStartError::Connect",
            Self::PeerAuthorization(_) => "ConnectOrStartError::PeerAuthorization",
            Self::Authentication(_) => "ConnectOrStartError::Authentication",
            Self::AuthenticationTimedOut => "ConnectOrStartError::AuthenticationTimedOut",
            Self::Handshake(_) => "ConnectOrStartError::Handshake",
            Self::HandshakeTimedOut => "ConnectOrStartError::HandshakeTimedOut",
            Self::DebugRequiresStoppedServer => "ConnectOrStartError::DebugRequiresStoppedServer",
            Self::CompanionExited => "ConnectOrStartError::CompanionExited",
            Self::StartupTimedOut => "ConnectOrStartError::StartupTimedOut",
        })
    }
}

impl fmt::Display for ConnectOrStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.safe_description())
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
            | Self::DebugRequiresStoppedServer
            | Self::CompanionExited
            | Self::StartupTimedOut => None,
        }
    }
}

impl From<ControlError> for ConnectOrStartError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

pub async fn connect_existing() -> Result<Option<ConnectedServer>, ConnectOrStartError> {
    match ClientEndpoint::discover().map_err(ConnectOrStartError::Control)? {
        ClientEndpointDiscovery::Registered(endpoint) => {
            connect_registered_server(endpoint, Instant::now() + STARTUP_TIMEOUT, false).await
        }
        _ => Ok(None),
    }
}

pub async fn connect_or_start() -> Result<ConnectedServer, ConnectOrStartError> {
    connect_or_start_with_debug(false).await
}

pub async fn connect_or_start_with_debug(
    debug: bool,
) -> Result<ConnectedServer, ConnectOrStartError> {
    connect_or_start_until_mode(
        debug,
        None,
        Instant::now() + STARTUP_TIMEOUT,
        STARTUP_TIMEOUT,
        ClientEndpoint::discover,
        || eprintln!("{STARTING_FEEDBACK}"),
    )
    .await
}

#[cfg(test)]
async fn connect_or_start_until(
    companion: Option<PathBuf>,
    deadline: Instant,
    startup_timeout: Duration,
    discover: impl FnMut() -> Result<ClientEndpointDiscovery, ControlError>,
    report_starting: impl FnMut(),
) -> Result<ConnectedServer, ConnectOrStartError> {
    connect_or_start_until_mode(
        false,
        companion,
        deadline,
        startup_timeout,
        discover,
        report_starting,
    )
    .await
}

async fn connect_or_start_until_mode(
    debug: bool,
    mut companion: Option<PathBuf>,
    mut deadline: Instant,
    startup_timeout: Duration,
    mut discover: impl FnMut() -> Result<ClientEndpointDiscovery, ControlError>,
    mut report_starting: impl FnMut(),
) -> Result<ConnectedServer, ConnectOrStartError> {
    let mut launched_companion = false;
    let mut incomplete_control_since = None;
    let mut initialization_progress = StartupInitializationProgress::default();
    let mut child: Option<Child> = None;
    let mut companion_exited = false;
    let mut companion_exit_observed_at = None;
    loop {
        companion_exited |= reap_exited_child(&mut child)?;
        let discovery = discover()?;
        if companion_exited
            && matches!(
                &discovery,
                ClientEndpointDiscovery::Absent | ClientEndpointDiscovery::Incomplete
            )
        {
            // A competing initializer may be between creating the control root
            // and acquiring/publishing its stable lock. Give discovery the same
            // bounded grace used for incomplete control initialization.
            let observed = companion_exit_observed_at.get_or_insert_with(Instant::now);
            if observed.elapsed() >= INCOMPLETE_CONTROL_GRACE {
                return Err(ConnectOrStartError::CompanionExited);
            }
        } else {
            companion_exit_observed_at = None;
        }
        let discovery_is_starting = matches!(&discovery, ClientEndpointDiscovery::Starting);
        let (should_report_starting, left_starting) =
            initialization_progress.observe(discovery_is_starting);
        if should_report_starting {
            report_starting();
        }
        if left_starting {
            deadline = Instant::now() + startup_timeout;
        }
        if !discovery_is_starting && Instant::now() >= deadline {
            return Err(ConnectOrStartError::StartupTimedOut);
        }

        let mut startup_allowed = false;
        match discovery {
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
            ClientEndpointDiscovery::Starting => {
                if debug && !launched_companion {
                    return Err(ConnectOrStartError::DebugRequiresStoppedServer);
                }
                incomplete_control_since = None;
            }
            ClientEndpointDiscovery::Registered(endpoint) => {
                incomplete_control_since = None;
                let debug_child_matches = child
                    .as_ref()
                    .is_some_and(|child| child.id() == endpoint.server_process_id());
                if let Some(connection) =
                    connect_registered_server(endpoint, deadline, launched_companion).await?
                {
                    reap_exited_child(&mut child)?;
                    if debug && !debug_child_matches {
                        return Err(ConnectOrStartError::DebugRequiresStoppedServer);
                    }
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
            child = Some(spawn_companion(path, debug)?);
            launched_companion = true;
        }
        companion_exited |= reap_exited_child(&mut child)?;
        let next_discovery = Instant::now() + DISCOVERY_RETRY_DELAY;
        if initialization_progress.latest_discovery_was_starting {
            time::sleep_until(next_discovery).await;
        } else {
            time::sleep_until(next_discovery.min(deadline)).await;
        }
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

#[cfg(all(test, unix))]
mod readiness_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_failure_descriptions_are_actionable_and_redacted() {
        const SENSITIVE: &str = "sensitive path\u{1b}]52;c;clipboard";
        let cases = [
            (
                ConnectOrStartError::Control(ControlError::InvalidState { reason: SENSITIVE }),
                "ConnectOrStartError::Control",
                "local control state could not be validated; automatic replacement was refused",
            ),
            (
                ConnectOrStartError::Control(ControlError::SocketPathTooLong),
                "ConnectOrStartError::Control",
                "local Unix socket path is too long; use a shorter absolute HOME for a separate Morons profile; existing state is not moved",
            ),
            (
                ConnectOrStartError::Control(ControlError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    SENSITIVE,
                ))),
                "ConnectOrStartError::Control",
                "local control state could not be validated; automatic replacement was refused",
            ),
            (
                ConnectOrStartError::Control(ControlError::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    SENSITIVE,
                ))),
                "ConnectOrStartError::Control",
                "local control state could not be validated; automatic replacement was refused",
            ),
            (
                ConnectOrStartError::CompanionIo(io::Error::other(SENSITIVE)),
                "ConnectOrStartError::CompanionIo",
                "packaged server companion could not be loaded or launched; reinstall matching Morons binaries together",
            ),
            (
                ConnectOrStartError::CompanionInvalid { reason: SENSITIVE },
                "ConnectOrStartError::CompanionInvalid",
                "packaged server companion failed integrity validation; reinstall matching Morons binaries together",
            ),
            (
                ConnectOrStartError::Connect(io::Error::other(SENSITIVE)),
                "ConnectOrStartError::Connect",
                "registered local server could not be reached safely",
            ),
            (
                ConnectOrStartError::PeerAuthorization(io::Error::other(SENSITIVE)),
                "ConnectOrStartError::PeerAuthorization",
                "registered local server failed operating-system peer authorization; automatic replacement was refused",
            ),
            (
                ConnectOrStartError::Authentication(AuthenticationError::Frame(
                    morons_protocol::FrameError::Io(io::Error::other(SENSITIVE)),
                )),
                "ConnectOrStartError::Authentication",
                "registered local server failed mutual authentication; automatic replacement was refused",
            ),
            (
                ConnectOrStartError::AuthenticationTimedOut,
                "ConnectOrStartError::AuthenticationTimedOut",
                "registered local server mutual authentication timed out",
            ),
            (
                ConnectOrStartError::Handshake(HandshakeError::Frame(
                    morons_protocol::FrameError::Io(io::Error::other(SENSITIVE)),
                )),
                "ConnectOrStartError::Handshake",
                "registered local server failed protocol negotiation",
            ),
            (
                ConnectOrStartError::Handshake(HandshakeError::ProtocolVersionMismatch {
                    expected_protocol_version: 30,
                    received_protocol_version: 29,
                }),
                "ConnectOrStartError::Handshake",
                "client and running server use incompatible protocol versions; stop the server with its matching client before upgrading",
            ),
            (
                ConnectOrStartError::HandshakeTimedOut,
                "ConnectOrStartError::HandshakeTimedOut",
                "registered local server protocol negotiation timed out",
            ),
            (
                ConnectOrStartError::CompanionExited,
                "ConnectOrStartError::CompanionExited",
                "server companion exited before becoming available; run the matching morons-server directly to diagnose startup; do not delete or downgrade existing state",
            ),
            (
                ConnectOrStartError::StartupTimedOut,
                "ConnectOrStartError::StartupTimedOut",
                "server companion did not become available before the startup timeout",
            ),
        ];

        for (error, debug, description) in cases {
            assert_eq!(error.to_string(), description);
            assert_eq!(format!("{error:?}"), debug);
            assert!(!error.to_string().contains(SENSITIVE));
            assert!(!format!("{error:?}").contains(SENSITIVE));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_companion_is_reported_without_waiting_for_startup_timeout() {
        // With null stdin, the shell exits without executing commands. Even a
        // successful child exit is unexpected when no server became available.
        let budget = Duration::from_secs(60);
        let result = time::timeout(
            Duration::from_secs(5),
            connect_or_start_until(
                Some(PathBuf::from("/bin/sh")),
                Instant::now() + budget,
                budget,
                || Ok(ClientEndpointDiscovery::Absent),
                || {},
            ),
        )
        .await
        .expect("child exit should not wait for the startup deadline");
        assert!(matches!(result, Err(ConnectOrStartError::CompanionExited)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_companion_allows_a_competing_initializer_to_publish_its_lock() {
        let mut observations = 0;
        let mut reported_starting = false;
        let budget = Duration::from_secs(60);
        let result = time::timeout(
            Duration::from_secs(5),
            connect_or_start_until(
                Some(PathBuf::from("/bin/sh")),
                Instant::now() + budget,
                budget,
                || {
                    observations += 1;
                    match observations {
                        1 => Ok(ClientEndpointDiscovery::Absent),
                        2..=8 => Ok(ClientEndpointDiscovery::Incomplete),
                        9 => Ok(ClientEndpointDiscovery::Starting),
                        _ => Err(ControlError::InvalidState {
                            reason: "end of competing-initializer fixture",
                        }),
                    }
                },
                || reported_starting = true,
            ),
        )
        .await
        .expect("fixture should finish before its watchdog");
        assert!(reported_starting);
        assert!(matches!(
            result,
            Err(ConnectOrStartError::Control(ControlError::InvalidState {
                reason: "end of competing-initializer fixture"
            }))
        ));
    }

    #[test]
    fn initialization_feedback_is_reported_once_across_state_changes() {
        let mut progress = StartupInitializationProgress::default();

        assert_eq!(progress.observe(true), (true, false));
        assert_eq!(progress.observe(true), (false, false));
        assert_eq!(progress.observe(false), (false, true));
        assert_eq!(progress.observe(true), (false, false));
        assert_eq!(progress.observe(false), (false, true));
    }

    #[tokio::test]
    async fn starting_then_incomplete_has_a_fresh_bounded_deadline() {
        let started = Instant::now();
        let transition = started + Duration::from_millis(100);
        let budget = Duration::from_millis(100);
        let mut reports = 0;
        let result = time::timeout(
            Duration::from_secs(2),
            connect_or_start_until(
                Some(PathBuf::new()),
                started + Duration::from_millis(10),
                budget,
                || {
                    Ok(if Instant::now() < transition {
                        ClientEndpointDiscovery::Starting
                    } else {
                        ClientEndpointDiscovery::Incomplete
                    })
                },
                || reports += 1,
            ),
        )
        .await
        .expect("incomplete control must not wait indefinitely");
        assert!(matches!(result, Err(ConnectOrStartError::StartupTimedOut)));
        assert!(Instant::now() >= transition + budget);
        assert_eq!(reports, 1);
    }

    #[tokio::test]
    async fn invalid_discovery_after_starting_fails_closed() {
        let mut discoveries = 0;
        let mut reports = 0;
        let result = connect_or_start_until(
            Some(PathBuf::new()),
            Instant::now() + Duration::from_millis(100),
            Duration::from_millis(100),
            || {
                discoveries += 1;
                if discoveries == 1 {
                    Ok(ClientEndpointDiscovery::Starting)
                } else {
                    Err(ControlError::InvalidState {
                        reason: "invalid test discovery",
                    })
                }
            },
            || reports += 1,
        )
        .await;

        assert!(matches!(
            result,
            Err(ConnectOrStartError::Control(
                ControlError::InvalidState { .. }
            ))
        ));
        assert_eq!(discoveries, 2);
        assert_eq!(reports, 1);
    }

    #[tokio::test]
    async fn waiting_for_starting_is_cancellation_safe() {
        let task = tokio::spawn(async {
            connect_or_start_until(
                Some(PathBuf::new()),
                Instant::now() + Duration::from_millis(10),
                Duration::from_millis(10),
                || Ok(ClientEndpointDiscovery::Starting),
                || {},
            )
            .await
        });
        time::sleep(Duration::from_millis(100)).await;
        task.abort();
        match task.await {
            Err(error) => assert!(error.is_cancelled()),
            Ok(_) => panic!("Starting wait unexpectedly completed"),
        }
    }

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

    #[test]
    fn terminal_application_error_reports_socket_path_diagnosis() {
        let error = crate::TerminalApplicationError::Connect(ConnectOrStartError::Control(
            ControlError::SocketPathTooLong,
        ));
        assert_eq!(
            error.to_string(),
            "local server connection failed: local Unix socket path is too long; use a shorter absolute HOME for a separate Morons profile; existing state is not moved"
        );
        assert_eq!(format!("{error:?}"), "TerminalApplicationError::Connect");
    }
}
