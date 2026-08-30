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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let server = Arc::new(ServerEndpoint::bind()?);
    let application = Arc::new(ServerApplication::open(&server)?);
    let connection_permits = Arc::new(Semaphore::new(MAX_CLIENT_CONNECTIONS));
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
            result = tokio::signal::ctrl_c() => {
                result?;
                break;
            }
        }
    }

    connections.abort_all();
    while connections.join_next().await.is_some() {}
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
