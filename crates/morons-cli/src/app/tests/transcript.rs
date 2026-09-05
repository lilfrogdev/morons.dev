use super::*;

#[test]
fn transcript_auto_scroll_accounts_for_wrapped_lines() {
    let (session, mut run) = fixture_session_and_run();
    run.state = RunState::Succeeded;
    let mut app = AppState::new("test-server");
    app.open_session(
        session,
        vec![
            TranscriptEntry::UserMessage {
                id: run.user_message_id,
                run_id: run.id,
                text: "wrapped transcript content ".repeat(40),
                attachments: Vec::new(),
                created_at_milliseconds: 1,
            },
            TranscriptEntry::AssistantMessage {
                id: MessageId::from_bytes([0x45; 16]),
                run_id: run.id,
                service: OpenCodeService::Zen,
                model_id: "grok-4.6".to_owned(),
                text: "LATEST-ASSISTANT-OUTPUT".to_owned(),
                refusal: false,
                created_at_milliseconds: 2,
            },
        ],
        vec![run],
        None,
        None,
    )
    .expect("session should open");

    let backend = TestBackend::new(40, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("wrapped transcript should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();

    assert!(rendered.contains("LATEST-ASSISTANT-OUTPUT"));

    terminal.backend_mut().resize(28, 12);
    terminal
        .draw(|frame| app.render(frame))
        .expect("resized wrapped transcript should render");
    assert!(rendered_terminal(&terminal).contains("LATEST-ASSISTANT-OUTPUT"));
    assert!(app.transcript_viewport.follows_latest());
}

#[test]
fn transcript_history_scrolls_without_new_output_stealing_the_view() {
    let (session, run) = fixture_session_and_run();
    let mut entries = (1_u8..=16)
        .map(|index| TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([index; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: format!("TRANSCRIPT-{index:02}"),
            refusal: false,
            created_at_milliseconds: u64::from(index),
        })
        .collect::<Vec<_>>();
    entries.push(TranscriptEntry::AssistantMessage {
        id: MessageId::from_bytes([0x40; 16]),
        run_id: run.id,
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
        text: "LATEST-BEFORE-SCROLL".to_owned(),
        refusal: false,
        created_at_milliseconds: 40,
    });
    let mut app = AppState::new("test-server");
    app.open_session(session, entries, vec![run.clone()], None, None)
        .expect("session should open");

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("latest transcript should render");
    assert!(rendered_terminal(&terminal).contains("LATEST-BEFORE-SCROLL"));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
        AppAction::None
    );
    terminal
        .draw(|frame| app.render(frame))
        .expect("oldest transcript should render");
    assert!(rendered_terminal(&terminal).contains("TRANSCRIPT-01"));
    assert!(!app.transcript_viewport.follows_latest());

    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        cursor: session_cursor(run.session_id, 50),
        session_id: run.session_id,
        entry: TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([0x50; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: "NEWEST-WHILE-READING".to_owned(),
            refusal: false,
            created_at_milliseconds: 50,
        },
    })
    .expect("new transcript entry should apply");
    terminal
        .draw(|frame| app.render(frame))
        .expect("history should remain anchored");
    let history = rendered_terminal(&terminal);
    assert!(history.contains("TRANSCRIPT-01"));
    assert!(history.contains("new output"));
    assert!(!history.contains("NEWEST-WHILE-READING"));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        AppAction::None
    );
    terminal
        .draw(|frame| app.render(frame))
        .expect("latest transcript should render again");
    let latest = rendered_terminal(&terminal);
    assert!(latest.contains("NEWEST-WHILE-READING"));
    assert!(!latest.contains("new output"));
}

#[test]
fn transcript_windows_page_to_history_edges_and_defer_live_output() {
    let (session, run) = fixture_session_and_run();
    let older_cursor = transcript_cursor(session.id, 200, 50, 137);
    let mut app = AppState::new("test-server");
    app.open_session_window(TranscriptWindowData {
        summary: session.clone(),
        entries: vec![TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([0x81; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: "LATEST-WINDOW".to_owned(),
            refusal: false,
            created_at_milliseconds: 200,
        }],
        runs: vec![run.clone()],
        active_run_id: Some(run.id),
        active_command_id: None,
        older_cursor: Some(older_cursor),
        newer_cursor: None,
    })
    .expect("latest transcript window should open");
    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("latest window should render");

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)),
        AppAction::NavigateTranscript {
            session_id: session.id,
            navigation: TranscriptNavigation::Oldest,
        }
    );
    let newer_cursor = transcript_cursor(session.id, 200, 50, 64);
    app.install_transcript_window(
        TranscriptWindowData {
            summary: session.clone(),
            entries: vec![TranscriptEntry::AssistantMessage {
                id: MessageId::from_bytes([0x82; 16]),
                run_id: run.id,
                service: OpenCodeService::Zen,
                model_id: "grok-4.6".to_owned(),
                text: "OLDEST-WINDOW".to_owned(),
                refusal: false,
                created_at_milliseconds: 1,
            }],
            runs: vec![run.clone()],
            active_run_id: Some(run.id),
            active_command_id: None,
            older_cursor: None,
            newer_cursor: Some(newer_cursor),
        },
        TranscriptNavigation::Oldest,
    )
    .expect("oldest transcript window should install");
    terminal
        .draw(|frame| app.render(frame))
        .expect("oldest window should render");
    assert!(rendered_terminal(&terminal).contains("OLDEST-WINDOW"));
    assert!(rendered_terminal(&terminal).contains("history"));

    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        cursor: session_cursor(session.id, 51),
        session_id: session.id,
        entry: TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([0x83; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: "DEFERRED-LIVE-OUTPUT".to_owned(),
            refusal: false,
            created_at_milliseconds: 201,
        },
    })
    .expect("live output should be deferred while history is visible");
    assert_eq!(app.session.as_ref().expect("session").entries.len(), 1);
    let command_id = LocalCommandId::from_bytes([0x84; 16]);
    app.session.as_mut().expect("session").active_command_id = Some(command_id);
    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        cursor: session_cursor(session.id, 52),
        session_id: session.id,
        entry: TranscriptEntry::LocalCommand {
            id: MessageId::from_bytes([0x85; 16]),
            command_id,
            command: "printf deferred".to_owned(),
            context_visible: false,
            status: morons_protocol::LocalCommandStatus::Succeeded,
            exit_code: Some(0),
            signal: None,
            stdout: "deferred".to_owned(),
            stderr: String::new(),
            created_at_milliseconds: 202,
        },
    })
    .expect("deferred terminal command should update live activity");
    assert!(
        app.session
            .as_ref()
            .expect("session")
            .active_command_id
            .is_none()
    );
    assert_eq!(app.session.as_ref().expect("session").entries.len(), 1);
    terminal
        .draw(|frame| app.render(frame))
        .expect("deferred output state should render");
    let history = rendered_terminal(&terminal);
    assert!(history.contains("new output"));
    assert!(!history.contains("DEFERRED-LIVE-OUTPUT"));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        AppAction::NavigateTranscript {
            session_id: session.id,
            navigation: TranscriptNavigation::Latest,
        }
    );
}

#[test]
fn live_transcript_window_stays_bounded_and_requests_a_fresh_tail() {
    let (session, run) = fixture_session_and_run();
    let entries = (1_u8..=u8::try_from(MAX_CLIENT_TRANSCRIPT_ENTRIES).expect("bounded fixture"))
        .map(|index| TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([index; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: format!("ENTRY-{index}"),
            refusal: false,
            created_at_milliseconds: u64::from(index),
        })
        .collect();
    let mut app = AppState::new("test-server");
    app.open_session(
        session.clone(),
        entries,
        vec![run.clone()],
        Some(run.id),
        None,
    )
    .expect("full transcript window should open");

    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        cursor: session_cursor(session.id, 300),
        session_id: session.id,
        entry: TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([0x90; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: "AFTER-WINDOW-LIMIT".to_owned(),
            refusal: false,
            created_at_milliseconds: 300,
        },
    })
    .expect("live entry should rotate the bounded window");

    let open = app.session.as_ref().expect("session should remain open");
    assert_eq!(open.entries.len(), TRANSCRIPT_WINDOW_TARGET_ENTRIES + 1);
    assert!(app.requires_tail_refresh());
    assert_eq!(
        app.request_tail_refresh(),
        AppAction::NavigateTranscript {
            session_id: session.id,
            navigation: TranscriptNavigation::Latest,
        }
    );
    assert!(!app.requires_tail_refresh());
}

#[test]
fn transcript_viewport_uses_visible_page_and_preserves_entry_anchor_on_reflow() {
    let first = TranscriptBlockKey::Entry(MessageId::from_bytes([0x61; 16]));
    let second = TranscriptBlockKey::Entry(MessageId::from_bytes([0x62; 16]));
    let third = TranscriptBlockKey::Entry(MessageId::from_bytes([0x63; 16]));
    let mut viewport = TranscriptViewport::default();
    viewport.update_layout(80, 6, Some(vec![(first, 8), (second, 8)]));
    assert_eq!(viewport.top(), 10);
    assert_eq!(viewport.visible_block_range(), (1..2, 2));

    viewport.scroll_page_up();
    assert_eq!(viewport.top(), 5);
    viewport.scroll_lines_down(3);
    assert_eq!(viewport.top(), 8);
    viewport.note_content_changed();
    viewport.update_layout(40, 6, Some(vec![(first, 16), (second, 16), (third, 4)]));
    assert_eq!(viewport.top(), 16);
    assert_eq!(viewport.visible_block_range(), (1..2, 0));
    assert!(viewport.has_newer_output());

    viewport.scroll_lines_down(usize::MAX);
    assert!(viewport.follows_latest());
    assert!(!viewport.has_newer_output());
    assert_eq!(viewport.top(), 30);
}

#[test]
fn mouse_wheel_scrolls_in_rendered_line_steps() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    let first = TranscriptBlockKey::Entry(MessageId::from_bytes([0x71; 16]));
    app.transcript_viewport
        .update_layout(80, 6, Some(vec![(first, 20)]));
    assert_eq!(app.transcript_viewport.top(), 14);

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.transcript_viewport.top(), 11);
    assert!(!app.transcript_viewport.follows_latest());
    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.transcript_viewport.top(), 14);
    assert!(app.transcript_viewport.follows_latest());

    app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 1,
        row: 1,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.transcript_viewport.top(), 14);
}

#[test]
fn composer_stays_bottom_docked_and_grows_upward() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");

    app.handle_paste("short");
    let short_rows = render_rows(&mut app, 60, 24);
    let short_top = row_containing(&short_rows, "Message · Enter submit")
        .expect("short composer should render");
    let short_bottom = short_top + 2;

    app.prompt.clear();
    app.handle_paste("@");
    app.install_session_skills(
        app.session
            .as_ref()
            .expect("session should exist")
            .summary
            .id,
        vec![SkillSummary {
            name: "long".to_owned(),
            description: "Completion above the fixed composer".to_owned(),
            source: SkillSource::Project,
        }],
    )
    .expect("skills should install");
    let completion_rows = render_rows(&mut app, 60, 24);
    let completion_top = row_containing(&completion_rows, "Message · Enter submit")
        .expect("composer should render with completion");
    assert_eq!(completion_top, short_top);
    assert!(row_containing(&completion_rows, "Skills ·").is_some());

    app.prompt.clear();
    app.handle_paste(&"wrapped composer text ".repeat(40));
    let long_rows = render_rows(&mut app, 60, 24);
    let long_top =
        row_containing(&long_rows, "Message · Enter submit").expect("long composer should render");
    assert!(long_top < short_top);
    assert_eq!(
        long_top + usize::from(render::MAX_PROMPT_HEIGHT) - 1,
        short_bottom
    );
}

#[test]
fn restored_failed_run_is_rendered_after_its_last_transcript_entry() {
    let (session, mut run) = fixture_session_and_run();
    run.state = RunState::Failed;
    run.failure = Some(RunFailureKind::ProviderRejected);
    let mut app = AppState::new("test-server");
    app.open_session(
        session,
        vec![TranscriptEntry::UserMessage {
            id: run.user_message_id,
            run_id: run.id,
            text: "hello".to_owned(),
            attachments: Vec::new(),
            created_at_milliseconds: 1,
        }],
        vec![run],
        None,
        None,
    )
    .expect("failed session should open");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("failed session should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();

    let user = rendered.find("hello").expect("user message should render");
    let outcome = rendered
        .find("Run failed · Zen · grok-4.6")
        .expect("terminal outcome should render");
    assert!(user < outcome);
    assert_eq!(rendered.matches("Run failed").count(), 1);
    assert!(rendered.contains("Provider rejected the request"));
}

#[test]
fn live_failed_run_updates_status_with_safe_durable_classification() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(
        session,
        vec![TranscriptEntry::UserMessage {
            id: run.user_message_id,
            run_id: run.id,
            text: "use a tool".to_owned(),
            attachments: Vec::new(),
            created_at_milliseconds: 1,
        }],
        vec![run.clone()],
        Some(run.id),
        None,
    )
    .expect("active session should open");
    let mut failed = run;
    failed.state = RunState::Failed;
    failed.failure = Some(RunFailureKind::InvalidProviderOutput);

    app.apply_event(ApplicationEvent::SessionRunChanged {
        cursor: session_cursor(failed.session_id, 2),
        run: failed,
    })
    .expect("failed transition should apply");

    assert_eq!(
        app.status.as_str(),
        "Run failed · Zen · grok-4.6: Model output was invalid"
    );
    let view = app.session.as_ref().expect("session should remain open");
    assert!(view.active_run_id.is_none());
    assert_eq!(view.runs[0].state, RunState::Failed);
}

#[test]
fn terminal_run_presentations_cover_non_success_outcomes_only() {
    let (_, mut run) = fixture_session_and_run();
    for (state, failure, heading, detail) in [
        (
            RunState::Failed,
            Some(RunFailureKind::CredentialNotConfigured),
            "Run failed",
            "OpenCode credential is not configured",
        ),
        (
            RunState::Cancelled,
            None,
            "Run cancelled",
            "Cancellation completed",
        ),
        (
            RunState::Interrupted,
            None,
            "Run interrupted",
            "Run stopped before completion",
        ),
        (
            RunState::Uncertain,
            None,
            "Run outcome uncertain",
            "An external effect may have occurred; inspect the working directory",
        ),
    ] {
        run.state = state;
        run.failure = failure;
        let presentation =
            terminal_run_presentation(&run).expect("terminal outcome should be presented");
        assert_eq!(presentation.heading, heading);
        assert_eq!(presentation.detail, detail);
    }

    run.state = RunState::Succeeded;
    run.failure = None;
    assert!(terminal_run_presentation(&run).is_none());
}

#[test]
fn durable_assistant_message_replaces_transient_output() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run.clone()], Some(run.id), None)
        .expect("session should open");
    app.apply_event(ApplicationEvent::SessionAssistantDelta {
        session_id: run.session_id,
        run_id: run.id,
        sequence: 1,
        delta: "partial".to_owned(),
        refusal: false,
    })
    .expect("delta should apply");
    assert!(
        app.session
            .as_ref()
            .and_then(|view| view.transient.as_ref())
            .is_some()
    );

    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        cursor: session_cursor(run.session_id, 2),
        session_id: run.session_id,
        entry: TranscriptEntry::AssistantMessage {
            id: MessageId::from_bytes([0x44; 16]),
            run_id: run.id,
            service: OpenCodeService::Zen,
            model_id: "grok-4.6".to_owned(),
            text: "complete".to_owned(),
            refusal: false,
            created_at_milliseconds: 2,
        },
    })
    .expect("durable assistant should apply");

    let view = app.session.as_ref().expect("session should remain open");
    assert!(view.transient.is_none());
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].text.as_str(), "complete");
}
