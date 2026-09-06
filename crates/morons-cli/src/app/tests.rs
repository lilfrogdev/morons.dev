use morons_protocol::{
    ApplicationEvent, ApplicationSettings, MessageId, OpenCodeApiKey, OpenCodeCredentialStatus,
    OpenCodeModelCapabilities, OpenCodeModelRetention, OpenCodeModelSummary,
    OpenCodeModelTrainingUse, OpenCodeService, ProviderProtocol, RunFailureKind, RunId, RunState,
    RunSummary, SessionContextStatus, SessionEventCursor, SessionId, SessionSummary, SkillSource,
    SkillSummary, SubagentModelSetting, TranscriptEntry,
};
use ratatui::{Terminal, backend::TestBackend};
use ratatui_crossterm::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use super::*;
use crate::terminal::MAX_PROMPT_BYTES;

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
        protocol: morons_protocol::ProviderProtocol::Responses,
        protocol_revision: 1,
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

fn rendered_terminal(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn render_rows(app: &mut AppState, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal should initialize");
    terminal
        .draw(|frame| app.render(frame))
        .expect("application should render");
    terminal
        .backend()
        .buffer()
        .content
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn row_containing(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(needle))
}

fn transcript_cursor(
    session_id: SessionId,
    snapshot_entry_sequence: u64,
    snapshot_event_sequence: u64,
    boundary_entry_sequence: u64,
) -> TranscriptCursor {
    let mut bytes = [0_u8; 40];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..24].copy_from_slice(&snapshot_entry_sequence.to_be_bytes());
    bytes[24..32].copy_from_slice(&snapshot_event_sequence.to_be_bytes());
    bytes[32..].copy_from_slice(&boundary_entry_sequence.to_be_bytes());
    TranscriptCursor::from_bytes(bytes)
}

fn session_cursor(session_id: SessionId, sequence: u64) -> SessionEventCursor {
    let mut bytes = [0_u8; 24];
    bytes[..16].copy_from_slice(session_id.as_bytes());
    bytes[16..].copy_from_slice(&sequence.to_be_bytes());
    SessionEventCursor::from_bytes(bytes)
}

mod credentials;
mod input;
mod presentation;
mod selection;
mod sessions;
mod transcript;
