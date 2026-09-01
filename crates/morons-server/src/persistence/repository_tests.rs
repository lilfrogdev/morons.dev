use std::{fs, sync::Arc};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::{
    MutationRequestId, PersistenceError, RunModelSelection, RunOpenCodeService, SessionEventCursor,
    SessionEventPayload, SessionStore, WorkspaceState,
    backend::Backend,
    paths::{create_private_file, ensure_private_directory},
    tests::TestRoot,
    types::{import_repository_fingerprint, repository_source_path_digest},
};

#[tokio::test(flavor = "current_thread")]
async fn repository_import_is_isolated_idempotent_and_durable() {
    let application = TestRoot::new("repository-import-app");
    let source = TestRoot::new("repository-import-source");
    fs::create_dir(source.path().join("nested")).expect("nested source should be created");
    fs::create_dir(source.path().join("empty")).expect("empty source should be created");
    fs::create_dir(source.path().join(".git")).expect("Git control root should be created");
    fs::write(
        source.path().join("nested").join("main.rs"),
        b"fn main() {}\n",
    )
    .expect("source file should be written");
    fs::write(source.path().join(".gitignore"), b"target\n").expect("Git ignore should be written");
    fs::write(
        source.path().join(".git").join("config"),
        b"control-only-data\n",
    )
    .expect("Git control data should be written");
    #[cfg(unix)]
    fs::set_permissions(
        source.path().join("nested").join("main.rs"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("source executable bit should be set");

    let store =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0x11; 16]), None)
        .await
        .expect("session should be created");
    let import_id = MutationRequestId::from_bytes([0x12; 16]);
    let workspace = store
        .import_repository(
            import_id,
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("repository should import");
    assert_eq!(workspace.state, WorkspaceState::Ready);
    assert_eq!(workspace.file_count, 2);
    assert_eq!(workspace.logical_bytes, 20);

    let repository = application
        .path()
        .join("workspaces")
        .join(encode_hex(&session.workspace_id))
        .join("repository");
    let baseline = repository.join("baseline");
    let worktree = repository.join("worktree");
    assert_eq!(
        fs::read(baseline.join("nested").join("main.rs")).expect("baseline should be readable"),
        b"fn main() {}\n"
    );
    assert_eq!(
        fs::read(worktree.join("nested").join("main.rs")).expect("worktree should be readable"),
        b"fn main() {}\n"
    );
    assert!(!baseline.join(".git").exists());
    assert!(!worktree.join(".git").exists());
    assert!(baseline.join(".gitignore").exists());
    assert_eq!(
        fs::read(source.path().join(".git").join("config")).expect("source Git data should remain"),
        b"control-only-data\n"
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(worktree.join("nested").join("main.rs"))
            .expect("worktree metadata should be readable")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let retry = store
        .import_repository(
            import_id,
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("exact import retry should resolve");
    assert_eq!(retry, workspace);
    let conflict = store
        .import_repository(
            import_id,
            session.id,
            application.path().to_string_lossy().into_owned(),
        )
        .await
        .expect_err("conflicting import retry should fail");
    assert!(matches!(conflict, PersistenceError::RequestConflict));
    let duplicate = store
        .import_repository(
            MutationRequestId::from_bytes([0x13; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect_err("a second repository should fail");
    assert!(matches!(
        duplicate,
        PersistenceError::RepositoryAlreadyImported
    ));

    let database_path = application.path().join("data").join("sessions.sqlite3");
    let connection = rusqlite::Connection::open(&database_path)
        .expect("database should open for manifest verification");
    let manifest: String = connection
        .query_row(
            "SELECT lower(hex(manifest_digest)) FROM repository_import_requests
             WHERE request_id = ?1",
            [&import_id.as_bytes()[..]],
            |row| row.get(0),
        )
        .expect("manifest digest should be stored");
    assert_eq!(
        manifest,
        "dfad2325c26c0ab9cfa8f47194dc5e7c8cf404ae61f62cac9d9b764084a47b07"
    );
    drop(connection);
    let database = fs::read(database_path).expect("database should be readable");
    assert!(!contains_bytes(
        &database,
        source.path().to_string_lossy().as_bytes()
    ));
    assert!(!contains_bytes(&database, b"fn main() {}"));

    let snapshot = store
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("session snapshot should load");
    assert_eq!(snapshot.workspace, workspace);
    let events = store
        .read_session_events(session.id, SessionEventCursor::new(session.id, 0), 10)
        .await
        .expect("workspace events should replay");
    let states = events
        .events
        .into_iter()
        .filter_map(|event| match event.payload {
            SessionEventPayload::WorkspaceChanged(workspace) => Some(workspace.state),
            SessionEventPayload::TranscriptEntry(_) | SessionEventPayload::RunChanged(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        states,
        vec![WorkspaceState::Importing, WorkspaceState::Ready]
    );
    drop(store);

    let reopened =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should reopen"));
    let durable = reopened
        .list_session_transcript(session.id, None, 1)
        .await
        .expect("durable snapshot should load");
    assert_eq!(durable.workspace, workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn immutable_baseline_damage_fails_closed_on_restart() {
    let application = TestRoot::new("repository-baseline-damage-app");
    let source = TestRoot::new("repository-baseline-damage-source");
    fs::write(source.path().join("file.txt"), b"original").expect("source should be written");
    let store =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0x14; 16]), None)
        .await
        .expect("session should be created");
    store
        .import_repository(
            MutationRequestId::from_bytes([0x15; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("repository should import");
    drop(store);

    let baseline = application
        .path()
        .join("workspaces")
        .join(encode_hex(&session.workspace_id))
        .join("repository")
        .join("baseline")
        .join("file.txt");
    fs::write(baseline, b"tampered").expect("baseline should be damaged");
    let error = match SessionStore::open_at(application.path()) {
        Ok(store) => {
            drop(store);
            panic!("damaged baseline should fail closed");
        }
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn repository_import_fingerprint_corruption_fails_closed() {
    let application = TestRoot::new("repository-fingerprint-app");
    let source = TestRoot::new("repository-fingerprint-source");
    fs::write(source.path().join("file.txt"), b"original").expect("source should be written");
    let store =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0x16; 16]), None)
        .await
        .expect("session should be created");
    let request_id = MutationRequestId::from_bytes([0x17; 16]);
    store
        .import_repository(
            request_id,
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("repository should import");
    drop(store);

    let connection =
        rusqlite::Connection::open(application.path().join("data").join("sessions.sqlite3"))
            .expect("database should open for corruption");
    connection
        .execute(
            "UPDATE repository_import_requests SET source_path_digest = ?2 WHERE request_id = ?1",
            rusqlite::params![&request_id.as_bytes()[..], &[0_u8; 32][..]],
        )
        .expect("source path digest should be corrupted");
    drop(connection);
    let error = match SessionStore::open_at(application.path()) {
        Ok(store) => {
            drop(store);
            panic!("corrupt repository fingerprint should fail closed");
        }
        Err(error) => error,
    };
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn repository_import_rejects_a_nonpristine_session() {
    let application = TestRoot::new("repository-nonpristine-app");
    let source = TestRoot::new("repository-nonpristine-source");
    fs::write(source.path().join("file.txt"), b"content").expect("source should be written");
    let store =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0x18; 16]), None)
        .await
        .expect("session should be created");
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0x19; 16]),
            0,
            b"not-a-real-import-test-key".to_vec(),
        )
        .await
        .expect("test credential should be stored");
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0x1a; 16]),
            session.id,
            "make the session nonpristine".to_owned(),
            RunModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: "grok-4.6".to_owned(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
            },
        )
        .await
        .expect("run input should be accepted");
    let error = store
        .import_repository(
            MutationRequestId::from_bytes([0x1b; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect_err("nonpristine session should reject import");
    assert!(matches!(error, PersistenceError::WorkspaceNotPristine));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn links_fail_without_publishing_and_a_new_import_can_succeed() {
    let application = TestRoot::new("repository-link-app");
    let source = TestRoot::new("repository-link-source");
    std::os::unix::fs::symlink("missing-target", source.path().join("linked"))
        .expect("source link should be created");
    let store =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0x21; 16]), None)
        .await
        .expect("session should be created");
    let error = store
        .import_repository(
            MutationRequestId::from_bytes([0x22; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect_err("a link should reject import");
    assert!(matches!(error, PersistenceError::InvalidInput { .. }));
    fs::remove_file(source.path().join("linked")).expect("source link should be removed");
    fs::write(source.path().join("safe.txt"), b"safe").expect("safe source should be written");
    let workspace = store
        .import_repository(
            MutationRequestId::from_bytes([0x23; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("new import should succeed after not-applied outcome");
    assert_eq!(workspace.state, WorkspaceState::Ready);
}

#[tokio::test(flavor = "current_thread")]
async fn protected_application_root_cannot_be_imported() {
    let application = TestRoot::new("repository-protected-app");
    let store =
        Arc::new(SessionStore::open_at(application.path()).expect("session store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0x31; 16]), None)
        .await
        .expect("session should be created");
    let error = store
        .import_repository(
            MutationRequestId::from_bytes([0x32; 16]),
            session.id,
            application.path().to_string_lossy().into_owned(),
        )
        .await
        .expect_err("protected state should not import");
    assert!(matches!(error, PersistenceError::InvalidInput { .. }));
}

#[tokio::test(flavor = "current_thread")]
async fn startup_marks_an_undispatched_import_not_applied() {
    let application = TestRoot::new("repository-prepared-recovery-app");
    let source = TestRoot::new("repository-prepared-recovery-source");
    fs::write(source.path().join("durable.txt"), b"durable").expect("source should be written");
    let store = SessionStore::open_at(application.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x38; 16]), None)
        .await
        .expect("session should be created");
    drop(store);

    let request_id = MutationRequestId::from_bytes([0x39; 16]);
    let source_path = source.path().to_string_lossy().into_owned();
    let mut backend = Backend::open(application.path()).expect("backend should open");
    backend
        .prepare_repository_import(
            request_id,
            import_repository_fingerprint(session.id, &source_path),
            repository_source_path_digest(&source_path),
            session.id,
        )
        .expect("import should prepare");
    drop(backend);

    let reopened = Arc::new(
        SessionStore::open_at(application.path())
            .expect("startup should reconcile prepared import"),
    );
    let workspace = reopened
        .workspace_summary(session.id)
        .await
        .expect("workspace summary should load");
    assert_eq!(workspace.state, WorkspaceState::Empty);
    let error = reopened
        .import_repository(request_id, session.id, source_path)
        .await
        .expect_err("exact retry should retain not-applied result");
    assert!(matches!(
        error,
        PersistenceError::RepositoryImportNotApplied
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn startup_blocks_ambiguous_import_state_without_reading_source() {
    let application = TestRoot::new("repository-blocked-recovery-app");
    let source = TestRoot::new("repository-blocked-recovery-source");
    fs::write(source.path().join("durable.txt"), b"durable").expect("source should be written");
    let store = SessionStore::open_at(application.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x3a; 16]), None)
        .await
        .expect("session should be created");
    drop(store);

    let request_id = MutationRequestId::from_bytes([0x3b; 16]);
    let source_path = source.path().to_string_lossy().into_owned();
    let mut backend = Backend::open(application.path()).expect("backend should open");
    let plan = backend
        .prepare_repository_import(
            request_id,
            import_repository_fingerprint(session.id, &source_path),
            repository_source_path_digest(&source_path),
            session.id,
        )
        .expect("import should prepare");
    let plan = backend
        .dispatch_repository_import(plan)
        .expect("import should dispatch");
    let workspace = backend.paths.workspace_path(&plan.workspace_id);
    let staging = workspace.join(format!(
        ".repository-importing-{}",
        encode_hex(&plan.operation_id)
    ));
    ensure_private_directory(&staging).expect("staging should be created");
    let mut metadata = create_private_file(&staging.join("import-metadata"))
        .expect("invalid marker should be created privately");
    use std::io::Write as _;
    metadata
        .write_all(b"invalid-complete-marker")
        .expect("invalid marker should be written");
    metadata
        .sync_all()
        .expect("invalid marker should synchronize");
    drop(metadata);
    drop(backend);
    drop(source);

    let reopened = SessionStore::open_at(application.path())
        .expect("startup should isolate an ambiguous import to its session");
    let workspace = reopened
        .workspace_summary(session.id)
        .await
        .expect("workspace summary should load");
    assert_eq!(workspace.state, WorkspaceState::Blocked);
}

#[tokio::test(flavor = "current_thread")]
async fn startup_finishes_a_published_dispatched_import_without_source_access() {
    let application = TestRoot::new("repository-recovery-app");
    let source = TestRoot::new("repository-recovery-source");
    fs::write(source.path().join("durable.txt"), b"durable").expect("source should be written");
    let store = SessionStore::open_at(application.path()).expect("session store should open");
    let session = store
        .create_session(MutationRequestId::from_bytes([0x41; 16]), None)
        .await
        .expect("session should be created");
    drop(store);

    let request_id = MutationRequestId::from_bytes([0x42; 16]);
    let source_path = source.path().to_string_lossy().into_owned();
    let mut backend = Backend::open(application.path()).expect("backend should open");
    let plan = backend
        .prepare_repository_import(
            request_id,
            import_repository_fingerprint(session.id, &source_path),
            repository_source_path_digest(&source_path),
            session.id,
        )
        .expect("import should prepare");
    let plan = backend
        .dispatch_repository_import(plan)
        .expect("import should dispatch");
    backend
        .paths
        .import_repository(plan, &source_path)
        .expect("filesystem import should publish");
    drop(backend);
    drop(source);

    let reopened = SessionStore::open_at(application.path()).expect("startup should reconcile");
    let workspace = reopened
        .workspace_summary(session.id)
        .await
        .expect("workspace summary should load");
    assert_eq!(workspace.state, WorkspaceState::Ready);
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn encode_hex(bytes: &[u8; 16]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
