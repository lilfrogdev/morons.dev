use std::{error::Error, time::Duration};

use morons_cli::perform_handshake;
use morons_protocol::{ClientEndpoint, authenticate_client};
use tokio::time;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let endpoint = ClientEndpoint::load()?;
    let mut connection = time::timeout(CONNECT_TIMEOUT, endpoint.connect()).await??;

    endpoint.verify_connected_server(&connection)?;
    time::timeout(
        AUTHENTICATION_TIMEOUT,
        authenticate_client(
            &mut connection,
            endpoint.authentication_key(),
            endpoint.host_epoch(),
        ),
    )
    .await??;

    let server_version = time::timeout(
        HANDSHAKE_TIMEOUT,
        perform_handshake(&mut connection, env!("CARGO_PKG_VERSION")),
    )
    .await??;

    println!("connected to morons-server {server_version}");

    Ok(())
}
