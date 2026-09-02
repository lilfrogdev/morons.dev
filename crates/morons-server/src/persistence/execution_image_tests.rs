use std::{fs, sync::Arc};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::Connection;

use super::{
    ExecutionImageState, MutationRequestId, PersistenceError, SessionStore,
    backend::Backend,
    tests::TestRoot,
    types::{execution_image_source_path_digest, provision_execution_image_fingerprint},
};

#[tokio::test(flavor = "current_thread")]
async fn execution_image_is_private_idempotent_durable_and_replaceable() {
    let application = TestRoot::new("execution-image-app");
    let first = ImageSources::new("execution-image-first", b"first-registry");
    let store = Arc::new(SessionStore::open_at(application.path()).expect("store should open"));
    let request_id = MutationRequestId::from_bytes([0x51; 16]);
    let summary = store
        .provision_execution_image(request_id, first.toolchain_path(), first.cargo_path())
        .await
        .expect("image should provision");
    assert_eq!(summary.state, ExecutionImageState::Ready);
    assert_eq!(summary.file_count, 3);
    assert!(summary.logical_bytes > 0);

    let database_path = application.path().join("data/sessions.sqlite3");
    let connection = Connection::open(&database_path).expect("database should open");
    let generation: Vec<u8> = connection
        .query_row(
            "SELECT generation_id FROM current_execution_image WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("current image should exist");
    let generation: [u8; 16] = generation
        .try_into()
        .expect("generation should be fixed width");
    drop(connection);
    let content = application
        .path()
        .join("sandbox-images")
        .join(hexadecimal(&generation))
        .join("content");
    assert!(content.join(tool_name("cargo")).exists());
    assert!(content.join(tool_name("rustc")).exists());
    assert_eq!(
        fs::read(content.join("cargo/registry/cache/fixture.crate"))
            .expect("registry seed should be copied"),
        b"first-registry"
    );
    assert!(!content.join("cargo/credentials.toml").exists());
    assert!(!content.join("cargo/config.toml").exists());
    assert!(!content.join("cargo/registry/.git").exists());

    drop(first);
    let retry = store
        .provision_execution_image(
            request_id,
            "/source/no-longer-present/toolchain".to_owned(),
            "/source/no-longer-present/cargo".to_owned(),
        )
        .await
        .expect_err("a retry with changed path bytes should conflict");
    assert!(matches!(retry, PersistenceError::RequestConflict));

    let second = ImageSources::new("execution-image-second", b"second-registry");
    let replacement = store
        .provision_execution_image(
            MutationRequestId::from_bytes([0x52; 16]),
            second.toolchain_path(),
            second.cargo_path(),
        )
        .await
        .expect("replacement should provision");
    assert_eq!(replacement.state, ExecutionImageState::Ready);
    assert!(!content.exists(), "inactive generation should be removed");
    drop(store);

    let database = fs::read(database_path).expect("database should be readable");
    assert!(!contains_bytes(
        &database,
        second.toolchain_path().as_bytes()
    ));
    assert!(!contains_bytes(&database, second.cargo_path().as_bytes()));
    let reopened = SessionStore::open_at(application.path()).expect("store should reopen");
    assert_eq!(
        reopened
            .execution_image_summary()
            .await
            .expect("status should load"),
        replacement
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exact_ready_retry_does_not_reread_sources() {
    let application = TestRoot::new("execution-image-retry-app");
    let sources = ImageSources::new("execution-image-retry-source", b"registry");
    let toolchain = sources.toolchain_path();
    let cargo = sources.cargo_path();
    let store = Arc::new(SessionStore::open_at(application.path()).expect("store should open"));
    let request_id = MutationRequestId::from_bytes([0x53; 16]);
    let first = store
        .provision_execution_image(request_id, toolchain.clone(), cargo.clone())
        .await
        .expect("image should provision");
    drop(sources);
    let retry = store
        .provision_execution_image(request_id, toolchain, cargo)
        .await
        .expect("exact completed retry should not read sources");
    assert_eq!(retry, first);
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_image_is_not_dispatched_during_recovery() {
    let application = TestRoot::new("execution-image-prepared-app");
    let sources = ImageSources::new("execution-image-prepared-source", b"registry");
    let request_id = MutationRequestId::from_bytes([0x54; 16]);
    let toolchain = sources.toolchain_path();
    let cargo = sources.cargo_path();
    let toolchain_digest = execution_image_source_path_digest(1, &toolchain);
    let cargo_digest = execution_image_source_path_digest(2, &cargo);
    let mut backend = Backend::open(application.path()).expect("backend should open");
    backend
        .prepare_execution_image(
            request_id,
            provision_execution_image_fingerprint(toolchain_digest, cargo_digest),
            toolchain_digest,
            cargo_digest,
        )
        .expect("operation should prepare");
    drop(backend);
    drop(sources);

    let store = Arc::new(SessionStore::open_at(application.path()).expect("recovery should open"));
    assert_eq!(
        store
            .execution_image_summary()
            .await
            .expect("summary should load")
            .state,
        ExecutionImageState::Unconfigured
    );
    let error = store
        .provision_execution_image(request_id, toolchain, cargo)
        .await
        .expect_err("prepared retry should retain not-applied outcome");
    assert!(matches!(
        error,
        PersistenceError::ExecutionImageProvisionNotApplied
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn damaged_current_image_fails_closed_on_restart() {
    let application = TestRoot::new("execution-image-damage-app");
    let sources = ImageSources::new("execution-image-damage-source", b"registry");
    let store = Arc::new(SessionStore::open_at(application.path()).expect("store should open"));
    store
        .provision_execution_image(
            MutationRequestId::from_bytes([0x55; 16]),
            sources.toolchain_path(),
            sources.cargo_path(),
        )
        .await
        .expect("image should provision");
    drop(store);
    let generation = current_generation(application.path());
    fs::write(
        application
            .path()
            .join("sandbox-images")
            .join(hexadecimal(&generation))
            .join("content")
            .join(tool_name("rustc")),
        b"damaged",
    )
    .expect("image should be damaged");
    let error = match SessionStore::open_at(application.path()) {
        Ok(store) => {
            drop(store);
            panic!("damaged image should fail closed");
        }
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn image_sources_with_links_are_rejected_without_publication() {
    let application = TestRoot::new("execution-image-link-app");
    let sources = ImageSources::new("execution-image-link-source", b"registry");
    std::os::unix::fs::symlink("rustc", sources.toolchain.path().join("bin/linked"))
        .expect("link should be created");
    let store = Arc::new(SessionStore::open_at(application.path()).expect("store should open"));
    let error = store
        .provision_execution_image(
            MutationRequestId::from_bytes([0x56; 16]),
            sources.toolchain_path(),
            sources.cargo_path(),
        )
        .await
        .expect_err("link should reject image");
    assert!(matches!(error, PersistenceError::InvalidInput { .. }));
    assert_eq!(
        store
            .execution_image_summary()
            .await
            .expect("summary should load")
            .state,
        ExecutionImageState::Unconfigured
    );
}

struct ImageSources {
    toolchain: TestRoot,
    cargo: TestRoot,
}

impl ImageSources {
    fn new(label: &str, registry_bytes: &[u8]) -> Self {
        let toolchain = TestRoot::new(&format!("{label}-toolchain"));
        let cargo = TestRoot::new(&format!("{label}-cargo"));
        fs::create_dir(toolchain.path().join("bin")).expect("bin should be created");
        for name in [tool_name("cargo"), tool_name("rustc")] {
            let path = toolchain.path().join(&name);
            fs::write(&path, format!("fixture-{name}")).expect("tool should be written");
            #[cfg(unix)]
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("tool should be executable");
        }
        fs::create_dir(cargo.path().join("registry")).expect("registry should be created");
        fs::create_dir(cargo.path().join("registry/cache")).expect("cache should be created");
        fs::create_dir(cargo.path().join("registry/.git")).expect("Git state should be created");
        fs::write(
            cargo.path().join("registry/cache/fixture.crate"),
            registry_bytes,
        )
        .expect("registry fixture should be written");
        fs::write(cargo.path().join("registry/.git/config"), b"untrusted")
            .expect("Git fixture should be written");
        fs::write(cargo.path().join("credentials.toml"), b"must-not-copy")
            .expect("credential fixture should be written");
        fs::write(cargo.path().join("config.toml"), b"must-not-copy")
            .expect("config fixture should be written");
        Self { toolchain, cargo }
    }

    fn toolchain_path(&self) -> String {
        self.toolchain.path().to_string_lossy().into_owned()
    }

    fn cargo_path(&self) -> String {
        self.cargo.path().to_string_lossy().into_owned()
    }
}

fn tool_name(tool: &str) -> String {
    if cfg!(windows) {
        format!("bin/{tool}.exe")
    } else {
        format!("bin/{tool}")
    }
}

fn current_generation(application: &std::path::Path) -> [u8; 16] {
    let connection =
        Connection::open(application.join("data/sessions.sqlite3")).expect("database should open");
    connection
        .query_row(
            "SELECT generation_id FROM current_execution_image WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("current generation should exist")
}

fn hexadecimal(identifier: &[u8; 16]) -> String {
    identifier
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
