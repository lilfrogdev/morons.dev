use super::{
    MutationRequestId, PersistenceError, RunModelSelection, RunOpenCodeService, SessionStore,
    tests::TestRoot,
};
use morons_protocol::{DiffChangeKind, DiffNodeKind};
use std::{fs, sync::Arc};

async fn imported_store(prefix: &str) -> (TestRoot, TestRoot, Arc<SessionStore>, super::Session) {
    let application = TestRoot::new(&format!("{prefix}-app"));
    let source = TestRoot::new(&format!("{prefix}-source"));
    fs::create_dir(source.path().join("deleted-empty")).expect("directory should be created");
    fs::write(source.path().join("deleted.txt"), b"deleted\n").expect("file should be written");
    fs::write(source.path().join("modified.txt"), b"before\n").expect("file should be written");
    fs::write(source.path().join("mode.sh"), b"#!/bin/sh\n").expect("file should be written");
    let store = Arc::new(SessionStore::open_at(application.path()).expect("store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0xb1; 16]), None)
        .await
        .expect("session should be created");
    store
        .import_repository(
            MutationRequestId::from_bytes([0xb2; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("repository should import");
    (application, source, store, session)
}

#[tokio::test(flavor = "current_thread")]
async fn review_reports_bounded_file_directory_binary_and_text_changes() {
    let (_application, _source, store, session) = imported_store("review-complete").await;
    let active = store
        .active_worktree_path(session.workspace_id)
        .await
        .expect("active worktree should resolve");
    fs::create_dir(active.join("added-empty")).expect("directory should be created");
    fs::create_dir(active.join("added-dir")).expect("directory should be created");
    fs::write(active.join("added-dir/file.txt"), b"added\n").expect("file should be written");
    fs::remove_dir(active.join("deleted-empty")).expect("directory should be removed");
    fs::remove_file(active.join("deleted.txt")).expect("file should be removed");
    fs::write(
        active.join("modified.txt"),
        "after\n\u{202e}hidden\n".as_bytes(),
    )
    .expect("file should be modified");
    fs::write(active.join("binary.bin"), [0xff, 0xfe, 0xfd]).expect("binary should be written");
    fs::write(active.join("large.txt"), vec![b'a'; 17 * 1_024])
        .expect("large file should be written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = fs::metadata(active.join("mode.sh"))
            .expect("mode file should exist")
            .permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(active.join("mode.sh"), permissions).expect("mode should change");
    }

    let (changes, next, _generation) = store
        .review_diff(session.id, None, 50)
        .await
        .expect("review should succeed");
    assert!(next.is_none());
    let find = |path: &str| {
        changes
            .iter()
            .find(|change| change.path == path)
            .unwrap_or_else(|| panic!("missing change for {path}"))
    };
    assert_eq!(find("added-empty").new_kind, Some(DiffNodeKind::Directory));
    assert_eq!(find("added-dir/file.txt").kind, DiffChangeKind::Added);
    assert_eq!(
        find("deleted-empty").old_kind,
        Some(DiffNodeKind::Directory)
    );
    assert_eq!(find("deleted.txt").kind, DiffChangeKind::Deleted);
    let modified = find("modified.txt");
    assert_eq!(modified.kind, DiffChangeKind::Modified);
    let excerpt = modified
        .excerpt
        .as_deref()
        .expect("text should have an excerpt");
    assert!(!excerpt.contains('\u{202e}'));
    assert!(!excerpt.contains(active.to_string_lossy().as_ref()));
    assert!(find("binary.bin").binary);
    assert!(find("binary.bin").excerpt.is_none());
    assert!(!find("large.txt").binary);
    assert!(find("large.txt").excerpt.is_none());
    #[cfg(unix)]
    assert_eq!(find("mode.sh").kind, DiffChangeKind::ModeChanged);
}

#[tokio::test(flavor = "current_thread")]
async fn review_pagination_is_stable_and_changed_content_stales_the_cursor() {
    let (_application, _source, store, session) = imported_store("review-pages").await;
    let active = store
        .active_worktree_path(session.workspace_id)
        .await
        .expect("active worktree should resolve");
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(active.join(name), name.as_bytes()).expect("file should be written");
    }
    let (first, cursor, first_generation) = store
        .review_diff(session.id, None, 2)
        .await
        .expect("first page should succeed");
    assert_eq!(
        first
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        ["a.txt", "b.txt"]
    );
    let cursor = cursor.expect("another page should exist");
    let mut tampered = cursor.as_token().as_bytes().to_vec();
    let last = tampered.last_mut().expect("cursor should not be empty");
    *last = if *last == b'0' { b'1' } else { b'0' };
    let tampered = morons_protocol::DiffCursor::from_token(
        String::from_utf8(tampered).expect("cursor should remain UTF-8"),
    )
    .expect("tampered cursor should retain protocol shape");
    let error = store
        .review_diff(session.id, Some(tampered), 2)
        .await
        .expect_err("tampered cursor should fail closed");
    assert!(matches!(error, PersistenceError::ReviewCursorStale));
    let (second, next, second_generation) = store
        .review_diff(session.id, Some(cursor.clone()), 2)
        .await
        .expect("second page should succeed");
    assert_eq!(
        second
            .iter()
            .map(|change| change.path.as_str())
            .collect::<Vec<_>>(),
        ["c.txt"]
    );
    assert!(next.is_none());
    assert_eq!(first_generation, second_generation);
    fs::write(active.join("c.txt"), b"changed").expect("file should change");
    let error = store
        .review_diff(session.id, Some(cursor), 2)
        .await
        .expect_err("changed content should stale the cursor");
    assert!(matches!(error, PersistenceError::ReviewCursorStale));
}

#[tokio::test(flavor = "current_thread")]
async fn review_rejects_active_runs() {
    let (_application, _source, store, session) = imported_store("review-state").await;
    store
        .set_open_code_credential(
            MutationRequestId::from_bytes([0xb3; 16]),
            0,
            b"not-a-real-review-key".to_vec(),
        )
        .await
        .expect("credential should be set");
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0xb4; 16]),
            session.id,
            "keep this run active".to_owned(),
            RunModelSelection {
                service: RunOpenCodeService::Zen,
                model_id: "grok-4.6".to_owned(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
            },
        )
        .await
        .expect("run should be accepted");
    let error = store
        .review_diff(session.id, None, 10)
        .await
        .expect_err("active run should block review");
    assert!(matches!(error, PersistenceError::WorkspaceBusy));
}

#[tokio::test(flavor = "current_thread")]
async fn review_rejects_baseline_damage() {
    let (_application, _source, store, session) = imported_store("review-baseline").await;
    let baseline = store
        .paths
        .workspace_path(&session.workspace_id)
        .join("repository/baseline/modified.txt");
    fs::write(baseline, b"damaged\n").expect("baseline should be damaged");
    let error = store
        .review_diff(session.id, None, 10)
        .await
        .expect_err("damaged baseline should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn imported_executable_mode_is_baseline_state() {
    use std::os::unix::fs::PermissionsExt as _;
    let application = TestRoot::new("review-executable-app");
    let source = TestRoot::new("review-executable-source");
    let source_file = source.path().join("script.sh");
    fs::write(&source_file, b"#!/bin/sh\n").expect("script should be written");
    fs::set_permissions(&source_file, fs::Permissions::from_mode(0o700))
        .expect("source should be executable");
    let store = Arc::new(SessionStore::open_at(application.path()).expect("store should open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0xc1; 16]), None)
        .await
        .expect("session should be created");
    store
        .import_repository(
            MutationRequestId::from_bytes([0xc2; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("repository should import");
    let (changes, _, _) = store
        .review_diff(session.id, None, 10)
        .await
        .expect("unchanged review should succeed");
    assert!(changes.is_empty());
    let active = store
        .active_worktree_path(session.workspace_id)
        .await
        .expect("active worktree should resolve");
    fs::set_permissions(active.join("script.sh"), fs::Permissions::from_mode(0o600))
        .expect("executable mode should be removed");
    let (changes, _, _) = store
        .review_diff(session.id, None, 10)
        .await
        .expect("mode review should succeed");
    assert!(matches!(changes.as_slice(), [change] if change.kind == DiffChangeKind::ModeChanged));
}

#[tokio::test(flavor = "current_thread")]
async fn review_fails_closed_when_baseline_mode_provenance_is_unavailable() {
    let (application, _source, store, session) = imported_store("review-legacy-mode").await;
    drop(store);
    let connection = rusqlite::Connection::open(application.path().join("data/sessions.sqlite3"))
        .expect("database should open");
    connection
        .execute(
            "UPDATE repository_import_requests SET review_baseline_version = 0",
            [],
        )
        .expect("mode provenance should be removed");
    drop(connection);
    let store = SessionStore::open_at(application.path()).expect("store should reopen");
    let error = store
        .review_diff(session.id, None, 10)
        .await
        .expect_err("legacy mode provenance should fail closed");
    assert!(matches!(error, PersistenceError::ReviewUnavailable));
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn review_rejects_links_without_disclosing_the_target() {
    let (_application, source, store, session) = imported_store("review-link").await;
    let active = store
        .active_worktree_path(session.workspace_id)
        .await
        .expect("active worktree should resolve");
    std::os::unix::fs::symlink(source.path().join("modified.txt"), active.join("linked"))
        .expect("link should be created");
    let error = store
        .review_diff(session.id, None, 10)
        .await
        .expect_err("link should fail closed");
    assert!(matches!(error, PersistenceError::InvalidState { .. }));
}
