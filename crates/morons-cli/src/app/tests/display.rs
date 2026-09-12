use super::*;

#[test]
fn assembled_source_budget_rejects_oversized_windows() {
    let (session, run) = fixture_session_and_run();
    let entry = |id, length| TranscriptEntry::UserMessage {
        id: MessageId::from_bytes([id; 16]),
        run_id: run.id,
        text: "x".repeat(length),
        attachments: Vec::new(),
        created_at_milliseconds: 1,
    };
    let limit = crate::transcript_budget::MAX_SOURCE_BYTES;
    assert_eq!(
        crate::transcript_budget::entry_source_bytes(&entry(1, limit)),
        Some(limit)
    );
    assert_eq!(
        crate::transcript_budget::entry_source_bytes(&entry(1, limit + 1)),
        None
    );
    let mut app = AppState::new("test-server");
    assert!(
        app.open_session(
            session,
            vec![entry(1, limit / 2), entry(2, limit / 2 + 1)],
            vec![run],
            None,
            None,
        )
        .is_err()
    );
}

#[test]
fn overflowing_preview_does_not_resume_with_smaller_delta() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run.clone()], Some(run.id), None)
        .expect("session opens");
    let view = app.session.as_mut().expect("session view");
    view.append_delta(run.id, 1, "retained prefix", false)
        .expect("initial delta");
    view.append_delta(run.id, 2, &"x".repeat(MAX_TRANSIENT_DELTA_BYTES), false)
        .expect("overflow pauses preview");
    view.append_delta(run.id, 3, "LATER-SMALL-DELTA", false)
        .expect("later delta stays deferred");
    let transient = view.transient.as_ref().expect("preview retained");
    assert!(transient.truncated);
    assert_eq!(transient.presented.as_str(), "retained prefix");
    let rows = render_rows(&mut app, 80, 20);
    assert!(row_containing(&rows, "retained prefix").is_some());
    assert!(row_containing(&rows, "LATER-SMALL-DELTA").is_none());
}
