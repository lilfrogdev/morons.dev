use super::*;
use crate::{
    persistence::{
        ActivationOutcome, CompletedToolTurn, DispatchOutcome, MutationRequestId, ProviderUsage,
        RunInputContext, RunModelSelection, RunService, SessionId, SessionStore,
        backend::context_compaction::tests::{append_stopped, fixture},
        tests::TestRoot,
        types::submit_session_input_fingerprint,
    },
    tools::{ToolInput, ToolOutput, ToolPath, ToolResult, ValidatedProviderCall},
};
use std::time::Instant;

fn tool_selection() -> RunModelSelection {
    RunModelSelection {
        service: RunService::OpenAiChatGpt,
        model_id: "gpt-6-astra".into(),
        protocol_revision: 5,
        maximum_input_tokens: 96_000,
        maximum_output_tokens: 32_000,
        supports_tool_calls: true,
        supports_image_input: true,
    }
}

async fn tool_history_checkpoints(
    label: &str,
    count: u8,
    result_bytes: usize,
) -> (TestRoot, TestRoot, Backend, SessionId) {
    let root = TestRoot::new(label);
    let selected = TestRoot::new(&format!("{label}-directory"));
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    store
        .set_openai_credential(
            MutationRequestId::from_bytes([0xe0; 16]),
            0,
            crate::provider::openai_auth::OAuthTokens::fixture("synthetic", "refresh", expires),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xe1; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap()
        .id;
    drop(store);
    let mut backend = Backend::open(root.path()).unwrap();
    let model = tool_selection();
    let run = backend
        .accept_session_input(
            MutationRequestId::from_bytes([0xe2; 16]),
            submit_session_input_fingerprint(
                session,
                "synthetic tool history",
                model.service,
                &model.model_id,
            ),
            session,
            "synthetic tool history".into(),
            model,
            RunInputContext {
                skills: Default::default(),
                project: Default::default(),
                attachments: Vec::new(),
            },
        )
        .unwrap()
        .run;
    assert_eq!(
        backend.activate_run(run.id).unwrap(),
        ActivationOutcome::Active
    );
    for index in 1..=count {
        let context = backend.load_run_context(run.id).unwrap();
        let operation = match backend
            .prepare_provider_operation(
                run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .unwrap()
        {
            crate::persistence::PrepareOperationOutcome::Prepared(operation) => operation,
            _ => panic!("synthetic provider operation must prepare"),
        };
        assert_eq!(
            backend.mark_provider_dispatched(run.id, operation).unwrap(),
            DispatchOutcome::Dispatched
        );
        let path = ToolPath::parse("synthetic.txt").unwrap();
        let turn = backend
            .complete_provider_tool_turn(
                run.id,
                operation,
                CompletedToolTurn {
                    provider_response_id: format!("synthetic-response-{index}"),
                    commentary: None,
                    usage: ProviderUsage {
                        input_tokens: 10,
                        cached_input_tokens: 0,
                        cache_write_input_tokens: 0,
                        output_tokens: 3,
                        reasoning_output_tokens: 0,
                        total_tokens: 13,
                    },
                    calls: vec![ValidatedProviderCall {
                        provider_call_id: format!("synthetic-call-{index}"),
                        input: ToolInput::Read {
                            path: path.clone(),
                            offset: 1,
                            limit: 2,
                        },
                        opaque_continuation: None,
                    }],
                },
            )
            .unwrap();
        let call = &turn.calls[0];
        backend
            .prepare_tool_operation(run.id, call.call_id, call.operation_id, None)
            .unwrap();
        backend
            .mark_tool_dispatched(run.id, call.call_id, call.operation_id)
            .unwrap();
        backend
            .complete_tool_result(
                run.id,
                call.call_id,
                call.operation_id,
                ToolResult::Ok {
                    output: ToolOutput::Read {
                        path,
                        offset: 1,
                        next_offset: 2,
                        end_of_file: true,
                        text: "x".repeat(result_bytes),
                    },
                },
            )
            .unwrap();
        let high_water = backend
            .load_run_context(run.id)
            .unwrap()
            .current_entry_high_water;
        let digest = backend.context_digest_through(session, high_water).unwrap();
        let transaction = backend.connection.transaction().unwrap();
        let sequence = next_sequence(&transaction).unwrap();
        transaction.execute(
            "INSERT INTO context_checkpoints VALUES (?1, ?2, NULL, ?3, ?4, 4, 1, 'muse-spark-1.2', 'PARENT', 22, ?5, 1)",
            params![&[index; 16][..], session.as_bytes(), i64::try_from(high_water).unwrap(), &digest[..], sequence_to_sql(sequence).unwrap()],
        ).unwrap();
        transaction.commit().unwrap();
    }
    (root, selected, backend, session)
}
async fn checkpoints(
    label: &str,
    count: u8,
    bytes: usize,
) -> (TestRoot, TestRoot, Backend, SessionId) {
    let (root, selected, store, session) = fixture(label).await;
    for index in 1..=count {
        append_stopped(&store, session, index, "x".repeat(bytes)).await;
    }
    drop(store);
    let mut backend = Backend::open(root.path()).unwrap();
    for index in 1..=count {
        let digest = backend
            .context_digest_through(session, u64::from(index))
            .unwrap();
        let transaction = backend.connection.transaction().unwrap();
        let sequence = next_sequence(&transaction).unwrap();
        transaction.execute(
            "INSERT INTO context_checkpoints VALUES (?1, ?2, NULL, ?3, ?4, 4, 1, 'muse-spark-1.2', 'PARENT', 22, ?5, 1)",
            params![&[index; 16][..], session.as_bytes(), i64::from(index), &digest[..], sequence_to_sql(sequence).unwrap()],
        ).unwrap();
        transaction.commit().unwrap();
    }
    (root, selected, backend, session)
}

fn independent_validation(backend: &Backend) {
    let mut statement = backend.connection.prepare(
        "SELECT session_id, source_entry_high_water, source_digest, summary, estimated_summary_tokens FROM context_checkpoints ORDER BY session_id, source_entry_high_water"
    ).unwrap();
    let mut rows = statement.query([]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let session = SessionId::from_bytes(row.get(0).unwrap());
        let high_water = u64::try_from(row.get::<_, i64>(1).unwrap()).unwrap();
        let digest: [u8; 32] = row.get(2).unwrap();
        let summary: String = row.get(3).unwrap();
        let tokens: u32 = row.get(4).unwrap();
        assert_eq!(
            backend.context_digest_through(session, high_water).unwrap(),
            digest
        );
        assert_eq!(
            crate::persistence::run_types::conservative_input_token_estimate(
                summary.len() as u64,
                1
            ),
            Some(tokens)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn batched_checkpoints_validate_every_digest_across_batch_boundaries() {
    let (_root, _selected, backend, session) = checkpoints("checkpoint-batches", 34, 128).await;
    independent_validation(&backend);
    backend.validate_context_checkpoint_digests().unwrap();
    for index in [1_u8, 16, 32, 33, 34] {
        let digest = backend
            .context_digest_through(session, u64::from(index))
            .unwrap();
        backend.connection.execute("UPDATE context_checkpoints SET source_digest = zeroblob(32) WHERE checkpoint_id = ?1", [&[index; 16][..]]).unwrap();
        assert!(backend.validate_context_checkpoint_digests().is_err());
        backend
            .connection
            .execute(
                "UPDATE context_checkpoints SET source_digest = ?1 WHERE checkpoint_id = ?2",
                params![&digest[..], &[index; 16][..]],
            )
            .unwrap();
    }
    backend.validate_context_checkpoint_digests().unwrap();
    backend.connection.execute("UPDATE context_checkpoints SET estimated_summary_tokens = 1 WHERE source_entry_high_water = 33", []).unwrap();
    assert!(backend.validate_context_checkpoint_digests().is_err());
    backend
        .connection
        .execute(
            "UPDATE context_checkpoints SET estimated_summary_tokens = 22",
            [],
        )
        .unwrap();
    backend.connection.execute("UPDATE session_entries SET text = 'changed' WHERE session_id = ?1 AND entry_sequence = 1", [session.as_bytes()]).unwrap();
    assert!(backend.validate_context_checkpoint_digests().is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_validation_rejects_tampered_tool_result() {
    let (_root, _selected, backend, session) =
        tool_history_checkpoints("checkpoint-tool-tamper", 1, 128).await;
    backend.validate_context_checkpoint_digests().unwrap();
    backend
        .connection
        .execute(
            "UPDATE tool_operation_facts SET result_payload = X'0000' WHERE session_id = ?1 AND fact_kind IN (3, 4, 5, 6)",
            [session.as_bytes()],
        )
        .unwrap();
    assert!(backend.validate_context_checkpoint_digests().is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn batched_prefixes_preserve_hidden_commands_and_reject_invalid_ranges() {
    let (root, _selected, store, session) = fixture("checkpoint-hidden").await;
    append_stopped(&store, session, 1, "first".into()).await;
    let command = store
        .accept_local_command(
            crate::persistence::MutationRequestId::from_bytes([2; 16]),
            session,
            "hidden".into(),
            false,
        )
        .await
        .unwrap();
    assert!(store.activate_local_command(command.id).await.unwrap());
    store
        .complete_local_command(
            command.id,
            crate::tools::ToolResult::Ok {
                output: crate::tools::ToolOutput::Bash {
                    exit_code: Some(0),
                    signal: None,
                    stdout: "hidden output".into(),
                    stderr: String::new(),
                },
            },
        )
        .await
        .unwrap();
    append_stopped(&store, session, 3, "last".into()).await;
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    let prefixes: Vec<_> = (1..=3)
        .map(|high_water| {
            (
                high_water,
                backend.context_digest_through(session, high_water).unwrap(),
            )
        })
        .collect();
    backend
        .validate_context_prefixes(session, &prefixes)
        .unwrap();
    assert!(
        backend
            .validate_context_prefixes(SessionId::from_bytes([99; 16]), &prefixes)
            .is_err()
    );
    for high_water in [0, 4] {
        assert!(
            backend
                .validate_context_prefixes(session, &[(high_water, [0; 32])])
                .is_err()
        );
    }
    backend
        .connection
        .execute(
            "UPDATE local_commands SET command_text = 'tampered' WHERE session_id = ?1",
            [session.as_bytes()],
        )
        .unwrap();
    assert!(
        backend
            .validate_context_prefixes(session, &prefixes)
            .is_err()
    );
    backend
        .connection
        .execute(
            "UPDATE local_commands SET command_text = 'hidden' WHERE session_id = ?1",
            [session.as_bytes()],
        )
        .unwrap();
    backend
        .validate_context_prefixes(session, &prefixes)
        .unwrap();
    backend
        .connection
        .execute(
            "DELETE FROM session_entries WHERE session_id = ?1 AND entry_sequence = 1",
            [session.as_bytes()],
        )
        .unwrap();
    assert!(
        backend
            .validate_context_prefixes(session, &prefixes)
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_batches_do_not_cross_sessions() {
    let (root, selected, store, first) = fixture("checkpoint-sessions").await;
    let second = store
        .create_session_at(
            crate::persistence::MutationRequestId::from_bytes([90; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap()
        .id;
    append_stopped(&store, first, 1, "first session".into()).await;
    append_stopped(&store, second, 2, "second session".into()).await;
    drop(store);
    let mut backend = Backend::open(root.path()).unwrap();
    backend.validate_context_checkpoint_digests().unwrap();
    for (index, session) in [(1_u8, first), (2, second)] {
        let digest = backend.context_digest_through(session, 1).unwrap();
        let transaction = backend.connection.transaction().unwrap();
        let sequence = next_sequence(&transaction).unwrap();
        transaction.execute(
            "INSERT INTO context_checkpoints VALUES (?1, ?2, NULL, 1, ?3, 4, 1, 'muse-spark-1.2', 'PARENT', 22, ?4, 1)",
            params![&[index; 16][..], session.as_bytes(), &digest[..], sequence_to_sql(sequence).unwrap()],
        ).unwrap();
        transaction.commit().unwrap();
    }
    independent_validation(&backend);
    backend.validate_context_checkpoint_digests().unwrap();
    backend.connection.execute("UPDATE context_checkpoints SET source_digest = (SELECT source_digest FROM context_checkpoints WHERE session_id = ?1) WHERE session_id = ?2", params![first.as_bytes(), second.as_bytes()]).unwrap();
    assert!(backend.validate_context_checkpoint_digests().is_err());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "offline synthetic checkpoint-validation benchmark"]
async fn benchmark_checkpoint_validation() {
    let (_root, _selected, backend, _session) =
        checkpoints("checkpoint-benchmark", 48, 32_768).await;
    eprintln!("text fixture: checkpoints=48, entries=48, entry_bytes=1572864");
    for _ in 0..3 {
        let start = Instant::now();
        independent_validation(&backend);
        let independent = start.elapsed();
        let start = Instant::now();
        backend.validate_context_checkpoint_digests().unwrap();
        eprintln!(
            "text checkpoint validation: independent={independent:?}, current={:?}",
            start.elapsed()
        );
    }

    let (_root, _selected, backend, _session) =
        tool_history_checkpoints("checkpoint-tool-benchmark", 48, 32_768).await;
    eprintln!("tool fixture: checkpoints=48, tool_calls=48, tool_results=48, result_bytes=1572864");
    for _ in 0..3 {
        let start = Instant::now();
        independent_validation(&backend);
        let independent = start.elapsed();
        let start = Instant::now();
        backend.validate_context_checkpoint_digests().unwrap();
        eprintln!(
            "tool checkpoint validation: independent={independent:?}, current={:?}",
            start.elapsed()
        );
    }

    for (name, validate) in [
        (
            "policy",
            Backend::validate_data_use_policy as fn(&Backend) -> Result<(), PersistenceError>,
        ),
        ("task", Backend::validate_task_bindings),
        ("web", Backend::validate_web_bindings),
        ("checkpoints", Backend::validate_context_checkpoint_digests),
        ("maintenance", Backend::validate_maintenance_records),
    ] {
        let start = Instant::now();
        validate(&backend).unwrap();
        eprintln!("synthetic context validation {name}: {:?}", start.elapsed());
    }
}
