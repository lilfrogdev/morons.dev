use std::{error::Error, time::Duration};

use morons_protocol::{ServerEndpoint, authenticate_server, authorize_accepted_peer};
use morons_server::{handle_handshake, persistence::SessionStore};
use tokio::time;

const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let server = ServerEndpoint::bind()?;
    let _session_store = SessionStore::open(&server)?;

    println!("morons-server ready");

    loop {
        let mut connection = tokio::select! {
            result = server.accept() => result?,
            result = tokio::signal::ctrl_c() => {
                result?;
                break;
            }
        };

        if authorize_accepted_peer(&connection).is_err() {
            continue;
        }

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
            Ok(Err(_)) | Err(_) => continue,
        }

        match time::timeout(
            HANDSHAKE_TIMEOUT,
            handle_handshake(&mut connection, env!("CARGO_PKG_VERSION")),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => eprintln!("client handshake failed: {error}"),
            Err(_) => eprintln!("client handshake timed out"),
        }
    }

    Ok(())
}
