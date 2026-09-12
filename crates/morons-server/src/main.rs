use std::{error::Error, sync::Arc, time::Duration};

use interprocess::local_socket::tokio::Stream;
use morons_protocol::{ServerEndpoint, authenticate_server, authorize_accepted_peer};
use morons_server::{
    HandshakeOutcome, ServerApplication, handle_handshake, handle_local_owner_requests,
};
use tokio::{sync::Semaphore, task::JoinSet, time};

const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIENT_CONNECTIONS: usize = 32;
const CONNECTION_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

const USAGE: &str = "Usage: morons-server [--debug | --help]";

#[derive(Debug, PartialEq, Eq)]
enum StartupMode {
    Normal,
    Debug,
    Help,
}

fn parse_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<StartupMode, &'static str> {
    let mut args = args.into_iter();
    let mode = match args.next() {
        None => StartupMode::Normal,
        Some(arg) if arg == "--debug" => StartupMode::Debug,
        Some(arg) if arg == "--help" => StartupMode::Help,
        Some(_) => return Err(USAGE),
    };
    if args.next().is_some() {
        return Err(USAGE);
    }
    Ok(mode)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mode = parse_args(std::env::args_os().skip(1))?;
    if mode == StartupMode::Help {
        println!("{USAGE}");
        return Ok(());
    }
    let _debug_guard = if mode == StartupMode::Debug {
        Some(morons_server::debug_log::start().map_err(|_| "debug startup failed")?)
    } else {
        None
    };
    let mut server = ServerEndpoint::prepare()?;
    let application = Arc::new(ServerApplication::open(&server)?);
    server.publish()?;
    let server = Arc::new(server);
    let connection_permits = Arc::new(Semaphore::new(MAX_CLIENT_CONNECTIONS));
    let mut shutdown_requests = application.subscribe_shutdown_requests();
    let mut connections = JoinSet::new();

    println!("morons-server ready");

    loop {
        while let Some(result) = connections.try_join_next() {
            if let Err(error) = result {
                eprintln!("client connection task failed: {error}");
            }
        }

        tokio::select! {
            result = server.accept() => {
                let connection = result?;
                if authorize_accepted_peer(&connection).is_err() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&connection_permits).try_acquire_owned() else {
                    continue;
                };
                let server = Arc::clone(&server);
                let application = Arc::clone(&application);
                connections.spawn(async move {
                    let _permit = permit;
                    serve_connection(connection, &server, &application).await;
                });
            }
            changed = shutdown_requests.changed() => {
                if changed.is_err() || *shutdown_requests.borrow_and_update() {
                    break;
                }
            }
            result = tokio::signal::ctrl_c() => {
                result?;
                break;
            }
        }
    }

    application.shutdown().await;
    let drain = async {
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                eprintln!("client connection task failed during shutdown: {error}");
            }
        }
    };
    if time::timeout(CONNECTION_DRAIN_TIMEOUT, drain)
        .await
        .is_err()
    {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    Ok(())
}

async fn serve_connection(
    mut connection: Stream,
    server: &ServerEndpoint,
    application: &ServerApplication,
) {
    match time::timeout(
        AUTHENTICATION_TIMEOUT,
        authenticate_server(
            &mut connection,
            server.authentication_key(),
            server.host_epoch(),
        ),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(_)) | Err(_) => return,
    }

    match time::timeout(
        HANDSHAKE_TIMEOUT,
        handle_handshake(&mut connection, env!("CARGO_PKG_VERSION")),
    )
    .await
    {
        Ok(Ok(HandshakeOutcome::Accepted)) => {}
        Ok(Ok(HandshakeOutcome::Rejected)) => return,
        Ok(Err(error)) => {
            eprintln!("client handshake failed: {error}");
            return;
        }
        Err(_) => {
            eprintln!("client handshake timed out");
            return;
        }
    }

    if let Err(error) = handle_local_owner_requests(&mut connection, application).await {
        eprintln!("client application connection failed: {error}");
    }
}

#[cfg(test)]
mod debug_argument_tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn debug_arguments_are_explicit_and_closed() {
        for (args, expected) in [
            (vec![], Ok(StartupMode::Normal)),
            (vec!["--help"], Ok(StartupMode::Help)),
            (vec!["--debug"], Ok(StartupMode::Debug)),
            (vec!["--debug", "--debug"], Err(USAGE)),
            (vec!["--help", "--debug"], Err(USAGE)),
            (vec!["--debug", "extra"], Err(USAGE)),
            (vec!["unknown"], Err(USAGE)),
            (vec!["\u{1b}[31mprivate\nvalue"], Err(USAGE)),
            (vec![""], Err(USAGE)),
        ] {
            assert_eq!(parse_args(args.into_iter().map(OsString::from)), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn debug_non_unicode_argument_is_rejected() {
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(parse_args([OsString::from_vec(vec![0xff])]), Err(USAGE));
    }

    #[cfg(windows)]
    #[test]
    fn debug_non_unicode_argument_is_rejected() {
        use std::os::windows::ffi::OsStringExt;
        assert_eq!(parse_args([OsString::from_wide(&[0xd800])]), Err(USAGE));
    }
}
