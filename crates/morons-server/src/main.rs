use std::{error::Error, time::Duration};

use interprocess::local_socket::{ListenerOptions, tokio::prelude::*};
use morons_protocol::local_socket_name;
use morons_server::handle_handshake;
use tokio::time;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let listener = ListenerOptions::new()
        .name(local_socket_name()?)
        .create_tokio()?;

    println!("morons-server ready");

    loop {
        let mut connection = tokio::select! {
            result = listener.accept() => result?,
            result = tokio::signal::ctrl_c() => {
                result?;
                break;
            }
        };

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
