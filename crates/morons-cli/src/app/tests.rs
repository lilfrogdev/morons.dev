use morons_protocol::{
    ApplicationEvent, MessageId, OpenCodeApiKey, OpenCodeCredentialStatus,
    OpenCodeModelCapabilities, OpenCodeModelRetention, OpenCodeModelSummary,
    OpenCodeModelTrainingUse, OpenCodeService, RunFailureKind, RunId, RunState, RunSummary,
    SessionContextStatus, SessionEventCursor, SessionId, SessionSummary, SkillSource, SkillSummary,
    TranscriptEntry,
};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_crossterm::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::terminal::MAX_PROMPT_BYTES;

#[test]
fn trusted_local_onboarding_and_help_are_explicit_and_modal() {
    let mut app = AppState::new("test-server");
    app.information_dialog = Some(InformationDialog::TrustNotice);
    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("trust notice should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("not a sandbox"));
    assert!(rendered.contains("container, VM"));
    assert!(rendered.contains("restricted OS account"));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.information_dialog.is_none());
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.information_dialog, Some(InformationDialog::Help));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.information_dialog.is_none());
}

#[test]
fn rendering_strips_terminal_and_bidirectional_controls() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(
        session,
        vec![TranscriptEntry::UserMessage {
            id: run.user_message_id,
            run_id: run.id,
            text: "safe\u{1b}]8;;https://example.invalid\u{7}link\u{1b}]8;;\u{7}\u{202e}txt"
                .to_owned(),
            attachments: Vec::new(),
            created_at_milliseconds: 1,
        }],
        vec![run],
        None,
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

#[test]
fn prompt_paste_and_rendering_remain_bounded() {
    let (session, run) = fixture_session_and_run();
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
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
fn bang_prompts_execute_local_commands_without_requiring_a_model() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.handle_paste("!!  printf private");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::ExecuteLocalCommand {
            session_id: selected,
            ref command,
            context_visible: false,
        } if selected == session_id && command == "printf private"
    ));
    assert!(!format!("{action:?}").contains("printf private"));
}

#[test]
fn slash_context_controls_query_status_and_submit_manual_compaction() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");

    app.handle_paste("/context");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::ShowContext {
            session_id: selected,
            service: OpenCodeService::Zen,
            ref model_id,
        } if selected == session_id && model_id == "grok-4.6"
    ));
    app.context_status_loaded(SessionContextStatus {
        session_id,
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
        context_policy_version: 4,
        estimated_input_tokens: 12_000,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        compaction_threshold_tokens: 67_200,
        checkpoint_source_entry_high_water: Some(7),
        checkpoint_estimated_summary_tokens: Some(500),
    })
    .expect("context status should install");
    assert!(app.prompt.is_empty());
    assert_eq!(
        app.session
            .as_ref()
            .and_then(|session| session.context_status.as_ref())
            .map(|context| context.estimated_input_tokens),
        Some(12_000)
    );

    app.handle_paste("/compact preserve migration constraints");
    assert!(matches!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::SubmitInput {
            session_id: selected,
            ref text,
            ref attachments,
            ..
        } if selected == session_id
            && text == "/compact preserve migration constraints"
            && attachments.is_empty()
    ));
}

#[test]
fn at_prefix_opens_bounded_skill_completion_and_tab_inserts_exact_name() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    app.install_session_skills(
        session_id,
        vec![
            SkillSummary {
                name: "alpha".to_owned(),
                description: "Alpha workflow".to_owned(),
                source: SkillSource::Project,
            },
            SkillSummary {
                name: "skill-creator".to_owned(),
                description: "Create Agent Skills".to_owned(),
                source: SkillSource::Bundled,
            },
        ],
    )
    .expect("skills should install");
    app.handle_paste("@");
    assert_eq!(
        app.skill_completion().map(|(skills, _)| skills.len()),
        Some(2)
    );

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("skill completion should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("@alpha"));
    assert!(rendered.contains("@skill-creator"));

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.prompt.as_str(), "@skill-creator ");
    app.handle_paste("make a release skill");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::SubmitInput {
            session_id: selected,
            ref text,
            ..
        } if selected == session_id && text == "@skill-creator make a release skill"
    ));
}

#[test]
fn image_drafts_use_atomic_unique_markers_and_survive_unsupported_submission() {
    let (session, run) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_models(OpenCodeService::Zen, vec![fixture_model()])
        .expect("models should be valid");
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open");
    let image =
        morons_image::normalize_rgba(2, 2, vec![0x55; 16]).expect("fixture image should normalize");
    assert_eq!(
        sanitize_image_name("bad\u{202e}[name].png"),
        "bad__name_.png"
    );
    app.add_draft_image(image.clone(), Some("puppies.png"));
    app.add_draft_image(image, Some("puppies.png"));
    assert_eq!(app.prompt.as_str(), "[puppies.png][puppies (2).png]");
    app.backspace_prompt();
    assert_eq!(app.prompt.as_str(), "[puppies.png]");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.prompt.as_str(), "[puppies.png]");

    app.models[0].model.capabilities.image_input = true;
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let debug = format!("{action:?}");
    assert!(matches!(
        action,
        AppAction::SubmitInput {
            session_id: selected,
            ref attachments,
            ..
        } if selected == session_id
            && attachments.len() == 1
            && attachments[0].display_name == "puppies.png"
            && attachments[0].marker_start == 0
    ));
    assert!(!debug.contains("iVBOR"));
    assert!(debug.contains("attachments"));
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
fn session_browser_presents_the_bound_working_directory() {
    let (mut session, _) = fixture_session_and_run();
    session.working_directory = Some("/projects/example".to_owned());
    let mut other = session.clone();
    other.id = SessionId::from_bytes([0x77; 16]);
    other.display_name = Some("Same directory".to_owned());
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session, other])
        .expect("sessions should be presented");

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
    assert!(rendered.contains("shared directory"));
    assert!(app.sessions.iter().all(|session| session.shared_directory));
}

#[test]
fn session_browser_rename_is_bounded_modal_and_redacted() {
    let (session, _) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.rename_dialog.is_some());
    app.handle_paste("Renamed session");
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::RenameSession {
            session_id: selected,
            ref display_name,
        } if selected == session_id && display_name == "Renamed session"
    ));
    assert!(!format!("{action:?}").contains("Renamed session"));
    assert!(app.rename_dialog.is_none());
}

#[test]
fn session_browser_toggles_durable_archive_state() {
    let (session, _) = fixture_session_and_run();
    let session_id = session.id;
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        AppAction::SetSessionArchived {
            session_id,
            archived: true,
        }
    );
    app.sessions[0].summary.archived = true;
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        AppAction::SetSessionArchived {
            session_id,
            archived: false,
        }
    );
}

#[test]
fn session_browser_requires_archived_confirmation_before_deletion() {
    let (mut session, _) = fixture_session_and_run();
    let session_id = session.id;
    session.archived = true;
    let mut app = AppState::new("test-server");
    app.replace_sessions(vec![session])
        .expect("session should be presented");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.confirm_delete, Some(session_id));
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        AppAction::DeleteSession { session_id }
    );
    assert_eq!(app.confirm_delete, None);
}

#[test]
fn input_action_debug_omits_prompt_text() {
    let action = AppAction::SubmitInput {
        session_id: SessionId::from_bytes([0x55; 16]),
        text: "sensitive prompt text".to_owned(),
        attachments: Vec::new(),
        service: OpenCodeService::Zen,
        model_id: "grok-4.6".to_owned(),
    };
    let debug = format!("{action:?}");
    assert!(!debug.contains("sensitive prompt text"));
    assert!(debug.contains("text_bytes"));
}

#[test]
fn model_search_ranks_direct_identifier_matches_before_loose_subsequences() {
    let grok = PresentedModel::new(fixture_model());
    let mut spark = fixture_model();
    spark.id = "gpt-5.3-codex-spark".to_owned();
    spark.display_name = "GPT 5.3 Codex Spark".to_owned();
    let spark = PresentedModel::new(spark);
    assert!(
        model_search_score(&grok, "grok").expect("Grok should match")
            < model_search_score(&spark, "grok").expect("Spark is a loose fuzzy match")
    );
}

#[test]
fn slash_model_search_selects_and_preserves_one_global_default() {
    let (session, run) = fixture_session_and_run();
    let mut luna = fixture_model();
    luna.service = OpenCodeService::Go;
    luna.id = "gpt-5.6-luna".to_owned();
    luna.display_name = "GPT 5.6 Luna".to_owned();
    let mut grok = fixture_model();
    grok.service = OpenCodeService::Go;

    let mut app = AppState::new("test-server");
    app.install_default_model(Some(OpenCodeModelSelection {
        service: OpenCodeService::Go,
        model_id: "grok-4.6".to_owned(),
    }));
    app.replace_models(OpenCodeService::Go, vec![luna.clone(), grok])
        .expect("Go models should apply");
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some("grok-4.6")
    );
    app.open_session(session, Vec::new(), vec![run], None, None)
        .expect("session should open without restoring its historical model");
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some("grok-4.6")
    );

    app.handle_paste("/model");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.model_dialog.as_ref().map(|dialog| dialog.selected),
        Some(1)
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(
        app.model_dialog.as_ref().map(|dialog| dialog.selected),
        Some(0)
    );
    app.handle_paste(&"x".repeat(MAX_MODEL_SEARCH_BYTES + 20));
    assert_eq!(
        app.model_dialog
            .as_ref()
            .map(|dialog| dialog.query.len_bytes()),
        Some(MAX_MODEL_SEARCH_BYTES)
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.model_dialog.is_none());
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some("grok-4.6")
    );

    app.handle_paste("/model luna");
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        AppAction::None
    );
    assert!(app.prompt.is_empty());
    assert_eq!(
        app.model_dialog
            .as_ref()
            .map(|dialog| dialog.query.as_str()),
        Some("luna")
    );
    assert_eq!(app.model_dialog_matches(), vec![0]);

    let backend = TestBackend::new(100, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("model selector should render");
    let rendered = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Model search"));
    assert!(rendered.contains("gpt-5.6-luna"));
    assert!(!rendered.contains("grok-4.6"));
    for (width, height) in [(1, 1), (8, 3), (40, 10)] {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("small terminal should initialize");
        terminal
            .draw(|frame| app.render(frame))
            .expect("model selector should render in a small terminal");
    }

    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        AppAction::None
    );
    let action = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        action,
        AppAction::SetDefaultModel {
            service: OpenCodeService::Go,
            ref model_id,
        } if model_id == "gpt-5.6-luna"
    ));
    app.install_default_model(Some(OpenCodeModelSelection {
        service: OpenCodeService::Go,
        model_id: luna.id,
    }));
    app.close_session();
    let selected_before_tab = app.selected_model;
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        AppAction::None
    );
    assert_eq!(app.selected_model, selected_before_tab);
}

#[test]
fn unavailable_saved_default_falls_back_only_to_an_available_reviewed_model() {
    let mut available = fixture_model();
    available.service = OpenCodeService::Go;
    available.id = "gpt-5.6-luna".to_owned();
    available.display_name = "GPT 5.6 Luna".to_owned();
    let mut app = AppState::new("test-server");
    app.install_default_model(Some(OpenCodeModelSelection {
        service: OpenCodeService::Go,
        model_id: "grok-4.6".to_owned(),
    }));
    app.replace_models(OpenCodeService::Go, vec![available.clone()])
        .expect("Go models should apply");
    assert!(app.default_model_is_unavailable());
    assert_eq!(
        app.selected_model().map(|model| model.model.id.as_str()),
        Some(available.id.as_str())
    );
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
            archived: false,
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
            image_input: false,
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
