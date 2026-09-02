use morons_protocol::{
    ApplicationEvent, MessageId, OpenCodeApiKey, OpenCodeCredentialStatus,
    OpenCodeModelCapabilities, OpenCodeModelRetention, OpenCodeModelSummary,
    OpenCodeModelTrainingUse, OpenCodeService, RunId, RunState, RunSummary, SessionEventCursor,
    SessionId, SessionSummary, ToolKind, TranscriptEntry, WorkspaceBlockReason, WorkspaceState,
    WorkspaceSummary,
};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::terminal::MAX_PROMPT_BYTES;

#[test]
fn rendering_strips_terminal_and_bidirectional_controls() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(
        session,
        empty_workspace(),
        vec![TranscriptEntry::UserMessage {
            id: run.user_message_id,
            run_id: run.id,
            text: "safe\u{1b}]8;;https://example.invalid\u{7}link\u{1b}]8;;\u{7}\u{202e}txt"
                .to_owned(),
            created_at_milliseconds: 1,
        }],
        vec![run],
        None,
    )
    .expect("session should open");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("application should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();

    assert!(!rendered.contains('\u{1b}'));
    assert!(!rendered.contains('\u{202e}'));
    assert!(rendered.contains("safe]8;;https://example.invalidlink]8;;txt"));
}

#[test]
fn durable_assistant_message_replaces_transient_output() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(
        session,
        empty_workspace(),
        Vec::new(),
        vec![run.clone()],
        Some(run.id),
    )
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

#[test]
fn prompt_paste_and_rendering_remain_bounded() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, empty_workspace(), Vec::new(), vec![run], None)
        .expect("session should open");
    app.handle_paste(&format!(
        "{}\u{1b}\u{202e}",
        "x".repeat(MAX_PROMPT_BYTES + 100)
    ));
    assert_eq!(app.prompt.len_bytes(), MAX_PROMPT_BYTES);
    assert!(!app.prompt.as_str().contains('\u{1b}'));
    assert!(!app.prompt.as_str().contains('\u{202e}'));

    for (width, height) in [(1, 1), (8, 3), (40, 10)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
        terminal
            .draw(|frame| app.render(frame))
            .expect("small terminal should render without panic");
    }
}

#[test]
fn unknown_mutation_requires_exact_retry_or_abandonment() {
    let mut app = AppState::new("test-server");
    app.mark_pending(PendingOperation::CreateSession);
    app.mark_pending_unknown();

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
        AppAction::RetryPending
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        AppAction::AbandonPending
    );
}

#[test]
fn credential_entry_is_hidden_and_emits_generation_bound_actions() {
    let mut app = AppState::new("test-server");
    app.set_credential_status(OpenCodeCredentialStatus {
        configured: false,
        generation: 4,
    });
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    app.handle_paste("not-a-real-key");

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("credential dialog should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!rendered.contains("not-a-real-key"));
    assert!(rendered.contains("Input is hidden"));

    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let debug = format!("{action:?}");
    let AppAction::SetCredential {
        expected_generation,
        api_key,
    } = action
    else {
        panic!("credential entry should produce a set action");
    };
    assert_eq!(expected_generation, 4);
    assert_eq!(
        api_key,
        OpenCodeApiKey::new("not-a-real-key").expect("test credential should be valid")
    );
    assert!(!debug.contains("not-a-real-key"));
    assert!(app.credential_dialog.is_none());
}

#[test]
fn credential_replacement_and_removal_use_observed_generation() {
    let mut app = AppState::new("test-server");
    app.set_credential_status(OpenCodeCredentialStatus {
        configured: true,
        generation: 9,
    });
    let control_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);

    assert_eq!(app.handle_key(control_k), AppAction::None);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
        AppAction::None
    );
    app.handle_paste("replacement-test-key");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::SetCredential {
            expected_generation: 9,
            ..
        }
    ));

    assert_eq!(app.handle_key(control_k), AppAction::None);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        AppAction::RemoveCredential {
            expected_generation: 9
        }
    );
}

#[test]
fn uncertain_tool_effect_requires_explicit_acknowledgement_confirmation() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(
        session,
        WorkspaceSummary {
            state: WorkspaceState::Blocked,
            file_count: 1,
            logical_bytes: 8,
            block_reason: Some(WorkspaceBlockReason::UncertainToolEffect),
            blocked_run_id: Some(run.id),
            blocked_tool: Some(ToolKind::EditFile),
        },
        Vec::new(),
        vec![run.clone()],
        None,
    )
    .expect("blocked session should open");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
        AppAction::None
    );
    assert!(app.confirm_uncertainty);
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        AppAction::AcknowledgeToolUncertainty {
            session_id: run.session_id,
            run_id: run.id,
        }
    );
    assert!(!app.confirm_uncertainty);
}

#[test]
fn session_browser_presents_the_bound_working_directory() {
    let (mut session, _) = fixture_session_and_run();
    session.working_directory = Some("/projects/example".to_owned());
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");

    let backend = TestBackend::new(100, 12);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("session browser should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("/projects/example"));
}

#[test]
fn input_action_debug_omits_prompt_text() {
    let action = AppAction::SubmitInput {
        session_id: SessionId::from_bytes([0x55; 16]),
        text: "sensitive prompt text".to_owned(),
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
    };
    let debug = format!("{action:?}");
    assert!(!debug.contains("sensitive prompt text"));
    assert!(debug.contains("text_bytes"));
}

#[test]
fn selected_model_is_always_an_available_reviewed_pair() {
    let mut unavailable = fixture_model();
    unavailable.available = false;
    let available = OpenCodeModelSummary {
        service: OpenCodeService::Go,
        id: "gpt-5.6-luna".to_owned(),
        display_name: "GPT 5.6 Luna".to_owned(),
        available: true,
        ..fixture_model()
    };
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![unavailable])
        .expect("Zen models should apply");
    assert!(app.selected_model().is_none());
    app.replace_models(OpenCodeService::Go, vec![available.clone()])
        .expect("Go models should apply");
    let selected = app
        .selected_model()
        .expect("available model should be selected");
    assert_eq!(selected.model.service, OpenCodeService::Go);
    assert_eq!(selected.model.id, available.id);
    app.replace_models(OpenCodeService::Go, Vec::new())
        .expect("a failed catalog can remove stale availability");
    assert!(app.selected_model().is_none());
}

fn empty_workspace() -> WorkspaceSummary {
    WorkspaceSummary {
        state: WorkspaceState::Empty,
        file_count: 0,
        logical_bytes: 0,
        block_reason: None,
        blocked_run_id: None,
        blocked_tool: None,
    }
}

fn fixture_session_and_run() -> (SessionSummary, RunSummary) {
    let session_id = SessionId::from_bytes([0x11; 16]);
    let user_message_id = MessageId::from_bytes([0x22; 16]);
    let run = RunSummary {
        id: RunId::from_bytes([0x33; 16]),
        session_id,
        user_message_id,
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
        protocol_revision: 1,
        credential_generation: 1,
        context_policy_version: 1,
        tool_catalog_version: 0,
        tool_limits_version: 0,
        state: RunState::Active,
        cancellation_requested: false,
        failure: None,
        accepted_at_milliseconds: 1,
        updated_at_milliseconds: 1,
    };
    (
        SessionSummary {
            id: session_id,
            display_name: Some("Test session".to_owned()),
            working_directory: None,
            created_at_milliseconds: 1,
        },
        run,
    )
}

fn fixture_model() -> OpenCodeModelSummary {
    OpenCodeModelSummary {
        service: OpenCodeService::Zen,
        id: "grok-4.6".to_owned(),
        display_name: "Grok 4.6".to_owned(),
        available: true,
        responses_protocol_revision: 1,
        capabilities: OpenCodeModelCapabilities {
            text_input: true,
            text_output: true,
            reasoning: true,
            reasoning_continuation: false,
            tool_calls: true,
        },
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        training_use: OpenCodeModelTrainingUse::NotUsed,
        retention: OpenCodeModelRetention::None,
    }
}

fn session_cursor(session_id: SessionId, sequence: u64) -> SessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    SessionEventCursor::from_bytes(bytes)
}
