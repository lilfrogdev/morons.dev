use super::*;
use morons_protocol::{
    ApplicationRequest, ApplicationResponse, ClientMessage, MessageId, RunState, ServerMessage,
    read_client_message, write_server_message,
};
use tokio::io::DuplexStream;

fn cursor(session: SessionId, boundary: u64) -> TranscriptCursor {
    let mut bytes = [0; 40];
    bytes[..16].copy_from_slice(session.as_bytes());
    bytes[16..24].copy_from_slice(&12_u64.to_be_bytes());
    bytes[24..32].copy_from_slice(&100_u64.to_be_bytes());
    bytes[32..].copy_from_slice(&boundary.to_be_bytes());
    TranscriptCursor::from_bytes(bytes)
}

fn session() -> SessionSummary {
    SessionSummary {
        id: SessionId::from_bytes([1; 16]),
        display_name: None,
        working_directory: Some(
            std::env::temp_dir()
                .join("viewer-fixture")
                .to_string_lossy()
                .into_owned(),
        ),
        archived: false,
        created_at_milliseconds: 1,
    }
}

fn run(session: SessionId) -> RunSummary {
    RunSummary {
        id: RunId::from_bytes([2; 16]),
        session_id: session,
        user_message_id: MessageId::from_bytes([3; 16]),
        service: ModelService::Zen,
        model_id: "grok-4.6".to_owned(),
        protocol_revision: 1,
        credential_generation: 1,
        context_policy_version: 1,
        tool_catalog_version: 0,
        tool_limits_version: 0,
        state: RunState::Succeeded,
        cancellation_requested: false,
        failure: None,
        accepted_at_milliseconds: 1,
        updated_at_milliseconds: 1,
    }
}

#[derive(Clone, Copy)]
enum Fault {
    None,
    LookaheadRun,
    LookaheadSession,
    Oversize,
    Duplicate,
}

async fn serve(mut io: DuplexStream, fault: Fault) -> usize {
    let session = session();
    let mut requests = 0;
    while let Some(message) = read_client_message(&mut io).await.unwrap() {
        let ClientMessage::Request {
            request_id,
            request:
                ApplicationRequest::ListSessionTranscript {
                    session_id,
                    cursor: position,
                    direction,
                    limit,
                },
        } = message
        else {
            panic!("only transcript queries are allowed")
        };
        requests += 1;
        assert!(requests <= 24);
        assert_eq!(session_id, session.id);
        assert_eq!(limit, 1);
        let boundary =
            position.map(|cursor| u64::from_be_bytes(cursor.as_bytes()[32..].try_into().unwrap()));
        let mut entry = match direction {
            TranscriptPageDirection::Older => boundary.unwrap_or(13) - 1,
            TranscriptPageDirection::Newer => boundary.unwrap_or(0) + 1,
        };
        assert!((1..=12).contains(&entry));
        if requests == 2 && matches!(fault, Fault::Duplicate) {
            entry = 12;
        }
        let mut run = run(session.id);
        let mut page_session = session.clone();
        if requests == 9 && matches!(fault, Fault::LookaheadRun) {
            run.updated_at_milliseconds += 1;
        }
        if requests == 9 && matches!(fault, Fault::LookaheadSession) {
            page_session.display_name = Some("changed snapshot".to_owned());
        }
        let size = if matches!(fault, Fault::Oversize) {
            crate::transcript_budget::MAX_SOURCE_BYTES + 1
        } else {
            128 * 1024
        };
        let mut event = [0; 24];
        event[..16].copy_from_slice(session.id.as_bytes());
        event[16..].copy_from_slice(&100_u64.to_be_bytes());
        write_server_message(
            &mut io,
            &ServerMessage::response(
                request_id,
                ApplicationResponse::SessionTranscriptListed {
                    session: page_session,
                    entries: vec![TranscriptEntry::AssistantMessage {
                        id: MessageId::from_bytes([u8::try_from(entry).unwrap(); 16]),
                        run_id: run.id,
                        service: run.service,
                        model_id: run.model_id.clone(),
                        text: "x".repeat(size),
                        refusal: false,
                        created_at_milliseconds: entry,
                    }],
                    runs: vec![run],
                    active_run_id: None,
                    active_command_id: None,
                    older_cursor: (entry > 1).then(|| cursor(session.id, entry)),
                    newer_cursor: (entry < 12).then(|| cursor(session.id, entry)),
                    event_cursor: SessionEventCursor::from_bytes(event),
                },
            ),
        )
        .await
        .unwrap();
    }
    requests
}

fn ids(window: &TranscriptWindow) -> Vec<u8> {
    window
        .entries
        .iter()
        .map(|entry| transcript_id(entry).as_bytes()[0])
        .collect()
}

#[tokio::test]
async fn byte_limited_wire_windows_keep_adjacent_cursors_in_both_directions() {
    for forward in [false, true] {
        let (io, peer) = tokio::io::duplex(4096);
        let mut client = ApplicationClient::from_negotiated_connection(io);
        let exchange = async {
            let target = if forward {
                TranscriptWindowTarget::Oldest
            } else {
                TranscriptWindowTarget::Latest
            };
            let first = load_transcript_window(&mut client, session().id, target)
                .await
                .unwrap();
            assert_eq!(first.entries.len(), 8);
            assert_eq!(
                crate::transcript_budget::window_source_bytes(&first.entries),
                Some(1024 * 1024)
            );
            assert_eq!(first.runs.len(), 1);
            assert_eq!(
                ids(&first),
                if forward {
                    (1..=8).collect::<Vec<_>>()
                } else {
                    (5..=12).collect()
                }
            );
            let target = if forward {
                assert_eq!(first.older_cursor, None);
                assert_eq!(first.newer_cursor, Some(cursor(session().id, 8)));
                TranscriptWindowTarget::Newer(first.newer_cursor.unwrap())
            } else {
                assert_eq!(first.newer_cursor, None);
                assert_eq!(first.older_cursor, Some(cursor(session().id, 5)));
                TranscriptWindowTarget::Older(first.older_cursor.unwrap())
            };
            let second = load_transcript_window(&mut client, session().id, target)
                .await
                .unwrap();
            assert_eq!(
                ids(&second),
                if forward {
                    (9..=12).collect::<Vec<_>>()
                } else {
                    (1..=4).collect()
                }
            );
            let back = if forward {
                TranscriptWindowTarget::Older(second.older_cursor.unwrap())
            } else {
                TranscriptWindowTarget::Newer(second.newer_cursor.unwrap())
            };
            let repeated = load_transcript_window(&mut client, session().id, back)
                .await
                .unwrap();
            assert_eq!(ids(&repeated), ids(&first));
            drop(client);
        };
        let (_, requests) = time::timeout(Duration::from_secs(10), async {
            tokio::join!(exchange, serve(peer, Fault::None))
        })
        .await
        .unwrap();
        assert_eq!(requests, 21); //9 (one unconsumed lookahead) +4+8.
    }
}

#[tokio::test]
async fn invalid_lookahead_and_oversized_or_repeated_wire_entries_fail_closed() {
    for (fault, expected) in [
        (Fault::LookaheadRun, 9),
        (Fault::LookaheadSession, 9),
        (Fault::Oversize, 1),
        (Fault::Duplicate, 2),
    ] {
        let (io, peer) = tokio::io::duplex(4096);
        let mut client = ApplicationClient::from_negotiated_connection(io);
        let exchange = async {
            assert!(matches!(
                load_transcript_window(&mut client, session().id, TranscriptWindowTarget::Latest)
                    .await,
                Err(ApplicationClientError::EventScopeMismatch)
            ));
            drop(client);
        };
        let (_, count) = time::timeout(Duration::from_secs(10), async {
            tokio::join!(exchange, serve(peer, fault))
        })
        .await
        .unwrap();
        assert_eq!(count, expected);
    }
}
