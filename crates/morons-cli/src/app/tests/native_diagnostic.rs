use super::*;

#[test]
fn compaction_activity_preserves_pending_run_and_draft() {
    let (session, mut run) = fixture_session_and_run();
    run.state = RunState::Active;
    let mut app = AppState::new("test-server");
    app.information_dialog = None;
    app.open_session(
        session.clone(),
        Vec::new(),
        vec![run.clone()],
        Some(run.id),
        None,
    )
    .unwrap();
    app.handle_paste("preserve my draft");
    for active in [true, false] {
        app.apply_event(ApplicationEvent::SessionCompactionActivity {
            session_id: session.id,
            run_id: run.id,
            active,
        })
        .unwrap();
        assert_eq!(
            render_rows(&mut app, 150, 30)
                .join("\n")
                .contains("Compacting"),
            active
        );
        assert_eq!(app.prompt.as_str(), "preserve my draft");
        let view = app.session.as_ref().unwrap();
        assert_eq!(view.active_run_id, Some(run.id));
        assert_eq!(view.runs, vec![run.clone()]);
        assert!(view.entries.is_empty());
    }
    app.apply_event(ApplicationEvent::SessionCompactionActivity {
        session_id: session.id,
        run_id: run.id,
        active: true,
    })
    .unwrap();
    run.state = RunState::Succeeded;
    app.apply_event(ApplicationEvent::SessionRunChanged {
        cursor: SessionEventCursor::from_bytes({
            let mut bytes = [0; 24];
            bytes[..16].copy_from_slice(session.id.as_bytes());
            bytes[23] = 1;
            bytes
        }),
        run,
    })
    .unwrap();
    assert!(app.session.as_ref().unwrap().compacting_run.is_none());
}

#[test]
fn native_diagnostic_is_fixed_status_not_transcript_draft_model_or_retry() {
    let (session, mut run) = fixture_session_and_run();
    run.service = ModelService::OpenAiChatGpt;
    run.model_id = "gpt-5.5".into();
    run.state = RunState::Failed;
    run.failure = Some(morons_protocol::RunFailureKind::ProviderProtocol);
    let mut app = AppState::new("test-server");
    app.information_dialog = None;
    app.open_session(session.clone(), Vec::new(), vec![run.clone()], None, None)
        .unwrap();
    app.handle_paste("preserve my draft");
    let selected = app.selected_model;
    let event = ApplicationEvent::SessionNativeResponseDiagnostic {
        session_id: session.id,
        run_id: run.id,
        reason: morons_protocol::NativeResponseFailure::Sequence,
    };
    app.apply_event(event).unwrap();
    assert_eq!(app.prompt.as_str(), "preserve my draft");
    assert_eq!(app.selected_model, selected);
    assert!(app.pending.is_none());
    assert!(app.session.as_ref().unwrap().entries.is_empty());
    assert_eq!(app.session.as_ref().unwrap().runs, vec![run.clone()]);
    let text = render_rows(&mut app, 150, 30).join("\n");
    assert!(text.contains("Native response rejected (sequence). Nothing was retried"));
    assert!(!text.contains("PRIVATE"));
    let foreign = ApplicationEvent::SessionNativeResponseDiagnostic {
        session_id: session.id,
        run_id: RunId::from_bytes([0xab; 16]),
        reason: morons_protocol::NativeResponseFailure::Usage,
    };
    assert_eq!(
        app.apply_event(foreign),
        Err(UiStateError::ResourceScopeMismatch)
    );
}
