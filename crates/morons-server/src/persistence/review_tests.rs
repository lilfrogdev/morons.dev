use super::{MutationRequestId, SessionStore, tests::TestRoot};
use std::{fs, sync::Arc};

#[tokio::test(flavor = "current_thread")]
async fn diff_review_and_absent_destination_export_are_generation_bound() {
    let app = TestRoot::new("review-export-app");
    let source = TestRoot::new("review-export-source");
    fs::write(source.path().join("modified.txt"), b"before\n").expect("write");
    fs::write(source.path().join("deleted.txt"), b"delete\n").expect("write");
    let store = Arc::new(SessionStore::open_at(app.path()).expect("open"));
    let session = store
        .create_session(MutationRequestId::from_bytes([0xa1; 16]), None)
        .await
        .expect("session");
    store
        .import_repository(
            MutationRequestId::from_bytes([0xa2; 16]),
            session.id,
            source.path().to_string_lossy().into_owned(),
        )
        .await
        .expect("import");
    let active = store
        .active_worktree_path(session.workspace_id)
        .await
        .expect("active");
    fs::write(active.join("modified.txt"), b"after\n").expect("modify");
    fs::remove_file(active.join("deleted.txt")).expect("delete");
    fs::write(active.join("added.txt"), b"added\n").expect("add");
    let (changes, next, generation) = store
        .review_diff(session.id, None, 50)
        .await
        .expect("review");
    assert!(next.is_none());
    assert_eq!(changes.len(), 3);
    assert_eq!(
        changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        ["added.txt", "deleted.txt", "modified.txt"]
    );
    assert!(changes.iter().all(|c| c.excerpt.is_some()));
    let destination = source.path().join("exported");
    let request = MutationRequestId::from_bytes([0xa3; 16]);
    let summary = store
        .export_worktree(
            request,
            session.id,
            generation,
            destination.to_string_lossy().into_owned(),
        )
        .await
        .expect("export");
    assert_eq!(summary.file_count, 2);
    assert_eq!(
        fs::read(destination.join("modified.txt")).expect("read"),
        b"after\n"
    );
    assert!(!destination.join("deleted.txt").exists());
    let retry = store
        .export_worktree(
            request,
            session.id,
            generation,
            destination.to_string_lossy().into_owned(),
        )
        .await
        .expect("retry");
    assert_eq!(retry, summary);
}
