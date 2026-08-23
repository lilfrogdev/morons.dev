use std::{error::Error, time::Duration};

use interprocess::local_socket::tokio::{Stream, prelude::*};
use morons_cli::perform_handshake;
use morons_protocol::local_socket_name;
use tokio::time;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut connection =
        time::timeout(CONNECT_TIMEOUT, Stream::connect(local_socket_name()?)).await??;

    let server_version = time::timeout(
        HANDSHAKE_TIMEOUT,
        perform_handshake(&mut connection, env!("CARGO_PKG_VERSION")),
    )
    .await??;

    println!("connected to morons-server {server_version}");

    Ok(())
}
