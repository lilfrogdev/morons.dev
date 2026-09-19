use super::*;
use morons_protocol::{
    ClientMessage, ServerEndpoint, ServerMessage, authenticate_server, authorize_accepted_peer,
    read_client_message, write_server_message,
};
use std::{fs, os::unix::fs::DirBuilderExt, process::Command};

const FIXTURE_ENV: &str = "MORONS_CLI_READINESS_FIXTURE";

struct Fixture {
    child: Child,
    home: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn starting_becomes_authenticated_ready_without_launching() {
    if std::env::var_os(FIXTURE_ENV).is_none() {
        // HOME is set only on a dedicated test subprocess, never on the parallel test runner.
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let home = PathBuf::from(format!("/tmp/mcli-{:016x}", u64::from_ne_bytes(nonce)));
        fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lifecycle::readiness_tests::starting_becomes_authenticated_ready_without_launching",
                "--nocapture",
            ])
            .env("HOME", &home)
            .env(FIXTURE_ENV, "1")
            .spawn()
            .unwrap();
        let mut fixture = Fixture { child, home };
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = fixture.child.try_wait().unwrap() {
                assert!(status.success(), "isolated readiness fixture failed");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "readiness fixture timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let mut server = ServerEndpoint::prepare().unwrap();
    assert!(matches!(
        connect_or_start_with_debug(true).await,
        Err(ConnectOrStartError::DebugRequiresStoppedServer)
    ));
    let (starting, observed) = tokio::sync::oneshot::channel();
    let mut starting = Some(starting);
    let mut reports = 0;
    let mut discoveries = 0;
    let original_deadline = Instant::now() + Duration::from_millis(100);
    let startup_timeout = Duration::from_millis(500);
    let client = connect_or_start_until(
        Some(PathBuf::new()),
        original_deadline,
        startup_timeout,
        || {
            discoveries += 1;
            ClientEndpoint::discover()
        },
        || {
            reports += 1;
            if let Some(starting) = starting.take() {
                starting.send(()).unwrap();
            }
        },
    );
    let serve = async {
        observed.await.unwrap();
        time::sleep_until(original_deadline + Duration::from_millis(100)).await;
        assert!(Instant::now() >= original_deadline);
        server.publish().unwrap();
        let mut connection = server.accept().await.unwrap();
        authorize_accepted_peer(&connection).unwrap();
        authenticate_server(
            &mut connection,
            server.authentication_key(),
            server.host_epoch(),
        )
        .await
        .unwrap();
        assert_eq!(
            read_client_message(&mut connection).await.unwrap(),
            Some(ClientMessage::hello(env!("CARGO_PKG_VERSION")))
        );
        write_server_message(&mut connection, &ServerMessage::hello("readiness-fixture"))
            .await
            .unwrap();
    };
    let (connected, ()) = time::timeout(Duration::from_secs(8), async {
        tokio::join!(client, serve)
    })
    .await
    .expect("authenticated readiness should finish");
    let connected = connected.unwrap();
    assert_eq!(connected.server_version(), "readiness-fixture");
    assert!(!connected.launched_companion());
    assert_eq!(reports, 1);
    assert!(discoveries >= 2);

    let debug_client = connect_or_start_with_debug(true);
    let serve = async {
        let mut connection = server.accept().await.unwrap();
        authorize_accepted_peer(&connection).unwrap();
        authenticate_server(
            &mut connection,
            server.authentication_key(),
            server.host_epoch(),
        )
        .await
        .unwrap();
        read_client_message(&mut connection).await.unwrap();
        write_server_message(&mut connection, &ServerMessage::hello("readiness-fixture"))
            .await
            .unwrap();
    };
    let (result, ()) = time::timeout(Duration::from_secs(8), async {
        tokio::join!(debug_client, serve)
    })
    .await
    .unwrap();
    assert!(matches!(
        result,
        Err(ConnectOrStartError::DebugRequiresStoppedServer)
    ));
    assert!(matches!(
        ClientEndpoint::discover().unwrap(),
        ClientEndpointDiscovery::Registered(_)
    ));
}
