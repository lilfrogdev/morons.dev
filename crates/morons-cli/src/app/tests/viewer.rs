use super::*;
use crate::app::transcript::PresentedTranscriptEntry;

fn message(run: &RunSummary, id: u8, text: String) -> TranscriptEntry {
    TranscriptEntry::AssistantMessage {
        id: MessageId::from_bytes([id; 16]),
        run_id: run.id,
        service: run.service,
        model_id: run.model_id.clone(),
        text,
        refusal: false,
        created_at_milliseconds: 1,
    }
}

fn opened(text: String) -> (AppState, RunSummary) {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(
        session,
        vec![message(&run, 1, text)],
        vec![run.clone()],
        Some(run.id),
        None,
    )
    .unwrap();
    (app, run)
}

fn commit(app: &mut AppState, run: &RunSummary, id: u8, text: String) {
    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        cursor: session_cursor(run.session_id, u64::from(id)),
        session_id: run.session_id,
        entry: message(run, id, text),
    })
    .unwrap();
}

fn delta(app: &mut AppState, run: &RunSummary, sequence: u64, text: &str) {
    app.apply_event(ApplicationEvent::SessionAssistantDelta {
        session_id: run.session_id,
        run_id: run.id,
        sequence,
        delta: text.to_owned(),
        refusal: false,
    })
    .unwrap();
}

#[test]
fn real_viewer_reaches_text_past_every_old_cap_and_reflows_part_anchor() {
    let text = format!(
        "{}\n{}\nVISIBLE-END",
        "wide 界 e\u{301} 👩‍💻 ".repeat(3000),
        "line\n".repeat(1500)
    );
    let (mut app, _) = opened(text.clone());
    let entry = &app.session.as_ref().unwrap().entries[0];
    assert_eq!(entry.text.as_str(), text);
    assert!(entry.text.part_count() > 30);
    assert!(
        render_rows(&mut app, 30, 16)
            .join("\n")
            .contains("VISIBLE-END")
    );
    app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    render_rows(&mut app, 30, 16);
    app.transcript_viewport.scroll_lines_down(300);
    let old = app.transcript_viewport.visible_block_range().0.start;
    assert!(old > 0);
    render_rows(&mut app, 55, 22);
    assert_eq!(app.transcript_viewport.visible_block_range().0.start, old);
    assert!(!app.transcript_viewport.follows_latest());
    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert!(
        render_rows(&mut app, 55, 22)
            .join("\n")
            .contains("VISIBLE-END")
    );
}

#[test]
fn real_narrow_render_scrolls_beyond_u16_without_stranding_the_tail() {
    let (mut app, _) = opened(format!("{}\nZ", "x".repeat(128 * 1024 - 2)));
    let rows = render_rows(&mut app, 3, 16); // One content cell wide.
    assert!(app.transcript_viewport.content_height() > usize::from(u16::MAX));
    assert!(app.transcript_viewport.top() > usize::from(u16::MAX));
    assert!(rows.iter().any(|row| row.contains('Z')));
    assert!(app.transcript_viewport.visible_block_range().1 < 4096);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    assert!(!app.transcript_viewport.follows_latest());
    render_rows(&mut app, 3, 16);
    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    assert!(
        render_rows(&mut app, 3, 16)
            .iter()
            .any(|row| row.contains('Z'))
    );
}

#[test]
fn structural_part_newlines_do_not_drop_final_empty_lines_or_duplicate_outcomes() {
    let (mut app, mut run) = opened(format!("{}T\n\n", "\n".repeat(256)));
    render_rows(&mut app, 200, 20);
    assert_eq!(app.transcript_viewport.content_height(), 260); // Header +256+3 rows.
    run.state = RunState::Failed;
    run.failure = Some(RunFailureKind::ProviderRejected);
    app.apply_event(ApplicationEvent::SessionRunChanged {
        cursor: session_cursor(run.session_id, 2),
        run,
    })
    .unwrap();
    let rows = render_rows(&mut app, 200, 20);
    assert_eq!(app.transcript_viewport.content_height(), 264);
    // One in the transcript and one in the global status footer.
    assert_eq!(rows.join("\n").matches("Run failed · Zen").count(), 2);
    app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    let rows = render_rows(&mut app, 200, 20);
    assert!(rows[2].contains("Assistant"));
    assert!(!rows[3..15].join("\n").contains("Run failed"));
}

#[test]
fn stream_rebuilds_whole_escape_state_and_committed_text_replaces_overflow() {
    let (mut app, run) = opened("initial".to_owned());
    delta(
        &mut app,
        &run,
        1,
        &format!("{}\x1b]52;HIDDEN", "x".repeat(70_000)),
    );
    delta(&mut app, &run, 2, "-STILL-HIDDEN\x1b");
    delta(&mut app, &run, 3, "\\SAFE-END");
    let preview = app.session.as_ref().unwrap().transient.as_ref().unwrap();
    assert!(preview.presented.as_str().ends_with("SAFE-END"));
    assert!(!preview.presented.as_str().contains("HIDDEN"));
    assert!(!preview.truncated);
    assert!(
        render_rows(&mut app, 80, 20)
            .join("\n")
            .contains("SAFE-END")
    );
    delta(&mut app, &run, 4, &"o".repeat(MAX_TRANSIENT_DELTA_BYTES));
    delta(&mut app, &run, 5, "NEVER-JOIN-A-SUFFIX");
    let rows = render_rows(&mut app, 80, 20).join("\n");
    assert!(rows.contains("Preview paused"));
    assert!(!rows.contains("NEVER-JOIN"));
    commit(
        &mut app,
        &run,
        8,
        format!("{}\nCOMMITTED-END", "y".repeat(120_000)),
    );
    assert!(app.session.as_ref().unwrap().transient.is_none());
    let rows = render_rows(&mut app, 80, 20).join("\n");
    assert!(rows.contains("COMMITTED-END"));
    assert!(!rows.contains("Preview paused"));
}

#[test]
fn missing_delta_prefix_and_connection_loss_pause_until_commit() {
    let (mut app, run) = opened("initial".to_owned());
    delta(&mut app, &run, 2, "missing-first-delta");
    let preview = app.session.as_ref().unwrap().transient.as_ref().unwrap();
    assert!(preview.truncated);
    assert!(preview.presented.as_str().is_empty());
    commit(&mut app, &run, 3, "complete first response".to_owned());
    // Ordinal is per run, not per assistant message.
    delta(&mut app, &run, 3, "second response prefix");
    app.pause_transcript_preview();
    delta(&mut app, &run, 5, "gap after reconnect");
    let preview = app.session.as_ref().unwrap().transient.as_ref().unwrap();
    assert!(preview.truncated);
    assert_eq!(preview.presented.as_str(), "second response prefix");
    commit(&mut app, &run, 4, "complete second response".to_owned());
    delta(&mut app, &run, 6, "third response");
    assert!(
        !app.session
            .as_ref()
            .unwrap()
            .transient
            .as_ref()
            .unwrap()
            .truncated
    );
}

#[test]
fn transient_part_anchor_transfers_to_complete_message() {
    let (mut app, run) = opened("initial".to_owned());
    let text = "streamed line\n".repeat(2000);
    delta(&mut app, &run, 1, &text);
    render_rows(&mut app, 80, 20);
    app.transcript_viewport.scroll_to_top();
    app.transcript_viewport.scroll_lines_down(400);
    let range = app.transcript_viewport.visible_block_range();
    let top = app.transcript_viewport.top();
    commit(&mut app, &run, 9, text);
    render_rows(&mut app, 80, 20);
    assert_eq!(app.transcript_viewport.visible_block_range(), range);
    assert_eq!(app.transcript_viewport.top(), top);
}

#[test]
fn live_byte_pressure_keeps_contiguous_window_cursor_and_completion_bookkeeping() {
    let (session, run) = fixture_session_and_run();
    let older = transcript_cursor(session.id, 200, 200, 100);
    let entries = (1..=8)
        .map(|id| message(&run, id, "x".repeat(128 * 1024)))
        .collect();
    let mut app = AppState::new("test-server");
    app.open_session_window(TranscriptWindowData {
        summary: session.clone(),
        entries,
        runs: vec![run.clone()],
        active_run_id: Some(run.id),
        active_command_id: None,
        older_cursor: Some(older),
        newer_cursor: None,
    })
    .unwrap();
    delta(&mut app, &run, 1, "preview");
    commit(&mut app, &run, 9, "overflowing commit".to_owned());
    commit(&mut app, &run, 10, "later tiny commit".to_owned());
    delta(&mut app, &run, 2, "later delta");
    let command_id = LocalCommandId::from_bytes([0x77; 16]);
    app.session.as_mut().unwrap().active_command_id = Some(command_id);
    app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
        session_id: session.id,
        cursor: session_cursor(session.id, 301),
        entry: TranscriptEntry::LocalCommand {
            id: MessageId::from_bytes([11; 16]),
            command_id,
            command: "owned fixture".to_owned(),
            context_visible: false,
            status: morons_protocol::LocalCommandStatus::Succeeded,
            exit_code: Some(0),
            signal: None,
            stdout: String::new(),
            stderr: String::new(),
            created_at_milliseconds: 1,
        },
    })
    .unwrap();
    let view = app.session.as_ref().unwrap();
    assert_eq!(
        view.source_bytes,
        crate::transcript_budget::MAX_SOURCE_BYTES
    );
    assert_eq!(
        view.entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        (1..=8)
            .map(|id| MessageId::from_bytes([id; 16]))
            .collect::<Vec<_>>()
    );
    assert_eq!(view.older_cursor, Some(older));
    assert!(view.transient.is_none() && view.active_command_id.is_none());
    assert!(view.deferred_newer_output && app.requires_tail_refresh());
    assert_eq!(
        app.apply_event(ApplicationEvent::SessionTranscriptEntryCommitted {
            session_id: session.id,
            cursor: session_cursor(session.id, 302),
            entry: message(
                &run,
                12,
                "x".repeat(crate::transcript_budget::MAX_SOURCE_BYTES + 1)
            ),
        }),
        Err(UiStateError::ResourceLimitExceeded)
    );
    app.install_transcript_window(
        TranscriptWindowData {
            summary: session,
            entries: vec![message(&run, 10, "refreshed".to_owned())],
            runs: vec![run.clone()],
            active_run_id: Some(run.id),
            active_command_id: None,
            older_cursor: Some(older),
            newer_cursor: None,
        },
        TranscriptNavigation::Latest,
    )
    .unwrap();
    assert!(!app.requires_tail_refresh());
    delta(
        &mut app,
        &run,
        3,
        "unreplayable suffix after fresh snapshot",
    );
    assert!(
        app.session
            .as_ref()
            .unwrap()
            .transient
            .as_ref()
            .unwrap()
            .truncated
    );
    commit(&mut app, &run, 13, "FULL-REFRESHED-COMMIT".to_owned());
    assert!(
        render_rows(&mut app, 80, 20)
            .join("\n")
            .contains("FULL-REFRESHED-COMMIT")
    );
}

#[test]
fn byte_pressure_does_not_auto_refresh_away_from_a_reader() {
    let (mut app, run) = opened("x".repeat(crate::transcript_budget::MAX_SOURCE_BYTES));
    render_rows(&mut app, 80, 20);
    app.transcript_viewport.scroll_page_up();
    let top = app.transcript_viewport.top();
    commit(&mut app, &run, 2, "newer".to_owned());
    assert!(!app.requires_tail_refresh());
    render_rows(&mut app, 80, 20);
    assert_eq!(app.transcript_viewport.top(), top);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
        AppAction::NavigateTranscript {
            session_id: run.session_id,
            navigation: TranscriptNavigation::Latest
        }
    );
}

#[test]
fn large_delivered_tool_and_command_shapes_and_grapheme_notice_are_admitted() {
    use morons_protocol::{ToolCallId, ToolKind, ToolResultStatus};
    let (_, run) = fixture_session_and_run();
    let tool = PresentedTranscriptEntry::new(TranscriptEntry::ToolResult {
        id: MessageId::from_bytes([4; 16]),
        run_id: run.id,
        call_id: ToolCallId::from_bytes([5; 16]),
        tool: ToolKind::ReadFile,
        status: ToolResultStatus::Succeeded,
        summary: format!("{}TOOL-END", "t".repeat(576 * 1024)),
        created_at_milliseconds: 1,
    })
    .unwrap();
    assert!(tool.text.as_str().ends_with("TOOL-END"));
    let command = PresentedTranscriptEntry::new(TranscriptEntry::LocalCommand {
        id: MessageId::from_bytes([6; 16]),
        command_id: LocalCommandId::from_bytes([7; 16]),
        command: "c".repeat(64 * 1024),
        stdout: "o".repeat(64 * 1024),
        stderr: "e".repeat(64 * 1024),
        status: morons_protocol::LocalCommandStatus::Interrupted,
        context_visible: false,
        exit_code: Some(i32::MIN),
        signal: Some(u16::MAX),
        created_at_milliseconds: 1,
    })
    .unwrap();
    assert_eq!(command.role, "Command !!");
    assert!(command.text.as_str().ends_with(&"e".repeat(64 * 1024)));
    let (mut app, _) = opened(format!("a{}", "\u{301}".repeat(65)));
    let rows = render_rows(&mut app, 100, 24).join("\n");
    assert!(rows.contains("Oversized grapheme shown as Unicode escapes"));
    assert!(rows.contains("\\u{301}"));
}
