use super::super::{
    context_compaction::tests::{append_stopped, fixture},
    run_records::load_required_run,
};
use super::*;
use crate::persistence::{
    MutationRequestId, RunId, RunModelSelection, SessionStore, tests::TestRoot,
};

async fn populated() -> (TestRoot, TestRoot, Backend, SessionId, RunId) {
    let (root, selected, store, session) = fixture("maintenance").await;
    let mut run = None;
    for index in 1..=5 {
        run = Some(append_stopped(&store, session, index, format!("TURN_{index}")).await);
    }
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    (root, selected, backend, session, run.unwrap())
}

fn seed(backend: &mut Backend, run: RunId, source: u64) -> Result<[u8; 16], PersistenceError> {
    let run = load_required_run(&backend.connection, run)?;
    let id = super::super::records::random_identifier()?;
    let source_digest = backend.context_digest_through(run.session_id, source)?;
    let instruction_digest = backend.maintenance_profile(&run)?.unwrap();
    let through = backend.connection.query_row(
        "SELECT entry_high_water FROM session_run_states WHERE session_id = ?1",
        [&run.session_id.as_bytes()[..]],
        |row| nonnegative_integer_from_row(row, 0),
    )?;
    let data_use_sequence = backend.data_use_policy()?.sequence;
    let transaction = backend
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut job = Job {
        id,
        session: run.session_id,
        run,
        parent: None,
        source,
        through,
        source_digest,
        instruction_digest,
        binding_digest: [0; 32],
        policy: 1,
        data_use_sequence,
        state: State::Prepared,
        sequence: next_sequence(&transaction)?,
        time: current_time_milliseconds()?,
    };
    job.binding_digest = job.digest();
    transaction.execute(
        "INSERT INTO compaction_maintenance_jobs (job_id, session_id, trigger_run_id, parent_checkpoint_id,
            source_entry_high_water, prepared_entry_high_water, source_digest, instruction_digest, binding_digest,
            maintenance_policy_version, state, prepared_sequence, prepared_at_milliseconds, data_use_sequence)
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, 1, 1, ?9, ?10, ?11)",
        params![&id[..], &job.session.as_bytes()[..], &job.run.id.as_bytes()[..], sequence_to_sql(source)?, sequence_to_sql(through)?,
            &source_digest[..], &instruction_digest[..], &job.binding_digest[..], sequence_to_sql(job.sequence)?, time_to_sql(job.time)?, sequence_to_sql(data_use_sequence)?],
    )?;
    transaction.commit()?;
    backend.validate_maintenance_records()?;
    Ok(id)
}

fn result(summary: &str) -> String {
    serde_json::json!({ "summary": summary, "input_tokens": 100, "cached_input_tokens": 20,
        "cache_write_input_tokens": 10, "output_tokens": 20, "reasoning_output_tokens": 5, "total_tokens": 120 }).to_string()
}

fn advance(backend: &mut Backend, id: [u8; 16], state: State) -> Result<(), PersistenceError> {
    let job = Job::load(&backend.connection, id)?;
    let payload = (state == State::Ready).then(|| result("READY_SUMMARY"));
    let transaction = backend
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    transition(&transaction, &job, state, payload.as_deref())?;
    transaction.commit()?;
    Ok(())
}

fn events(backend: &Backend, id: [u8; 16]) -> i64 {
    backend
        .connection
        .query_row(
            "SELECT COUNT(*) FROM compaction_maintenance_events WHERE job_id = ?1",
            [&id[..]],
            |row| row.get(0),
        )
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn restart_cancels_preparation_marks_dispatch_uncertain_and_preserves_committed_readiness() {
    for (state, expected, count) in [
        (State::Prepared, State::Cancelled, 1),
        (State::Dispatched, State::Uncertain, 2),
        (State::Ready, State::Ready, 2),
    ] {
        let (root, _selected, mut backend, _session, run) = populated().await;
        let id = seed(&mut backend, run, 3).unwrap();
        if state != State::Prepared {
            advance(&mut backend, id, State::Dispatched).unwrap();
        }
        if state == State::Ready {
            advance(&mut backend, id, State::Ready).unwrap();
        }
        backend.validate_maintenance_records().unwrap();
        drop(backend);
        for _ in 0..2 {
            let backend = Backend::open(root.path()).unwrap();
            assert_eq!(Job::load(&backend.connection, id).unwrap().state, expected);
            assert_eq!(events(&backend, id), count);
            let effects: i64 = backend.connection.query_row(
                "SELECT (SELECT COUNT(*) FROM context_checkpoints) + (SELECT COUNT(*) FROM provider_operation_facts) + (SELECT COUNT(*) FROM compaction_operations)", [], |row| row.get(0)).unwrap();
            assert_eq!(effects, 0);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn data_use_changes_block_stale_maintenance_without_fabricating_stopped_execution() {
    for state in [State::Prepared, State::Dispatched, State::Ready] {
        let (_root, _selected, mut backend, _session, run) = populated().await;
        backend.configure_maintenance(true).unwrap();
        let id = seed(&mut backend, run, 3).unwrap();
        if state != State::Prepared {
            advance(&mut backend, id, State::Dispatched).unwrap();
        }
        if state == State::Ready {
            advance(&mut backend, id, State::Ready).unwrap();
        }
        backend
            .set_data_use_policy(
                MutationRequestId::from_bytes([0xe7; 16]),
                0,
                Default::default(),
            )
            .unwrap();
        assert_eq!(Job::load(&backend.connection, id).unwrap().state, state);
        if state == State::Prepared {
            assert!(!backend.dispatch_maintenance(id).unwrap());
            assert_eq!(
                Job::load(&backend.connection, id).unwrap().state,
                State::Cancelled
            );
        } else {
            if state == State::Dispatched {
                advance(&mut backend, id, State::Ready).unwrap();
            }
            backend.recover_maintenance_jobs().unwrap();
            assert_eq!(
                Job::load(&backend.connection, id).unwrap().state,
                State::Discarded
            );
        }
        backend.validate_maintenance_records().unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_data_use_binding_rejects_a_different_policy_sequence() {
    let (_root, _selected, mut backend, _session, run) = populated().await;
    let policy = backend
        .set_data_use_policy(
            MutationRequestId::from_bytes([0xe7; 16]),
            0,
            Default::default(),
        )
        .unwrap();
    let id = seed(&mut backend, run, 3).unwrap();
    assert_eq!(
        Job::load(&backend.connection, id)
            .unwrap()
            .data_use_sequence,
        policy.sequence
    );
    backend
        .connection
        .execute(
            "UPDATE compaction_maintenance_jobs SET data_use_sequence = 0 WHERE job_id = ?1",
            [&id[..]],
        )
        .unwrap();
    assert!(backend.validate_maintenance_records().is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn transitions_are_closed_and_invalid_or_stale_writes_roll_back() {
    let (_root, _selected, mut backend, session, run) = populated().await;
    let id = seed(&mut backend, run, 3).unwrap();
    assert!(advance(&mut backend, id, State::Ready).is_err());
    assert_eq!(events(&backend, id), 0);
    let stale = Job::load(&backend.connection, id).unwrap();
    advance(&mut backend, id, State::Dispatched).unwrap();
    let before: i64 = backend
        .connection
        .query_row("SELECT next_value FROM logical_sequences", [], |row| {
            row.get(0)
        })
        .unwrap();
    {
        let transaction = backend
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(transition(&transaction, &stale, State::Cancelled, None).is_err());
    }
    let after: i64 = backend
        .connection
        .query_row("SELECT next_value FROM logical_sequences", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, after);
    assert!(advance(&mut backend, id, State::Cancelled).is_err());
    {
        let transaction = backend
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(drain_for_archive(&transaction, session).is_err());
        assert!(ensure_drained(&transaction, session).is_err());
    }
    advance(&mut backend, id, State::Ready).unwrap();
    {
        let transaction = backend
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        drain_for_archive(&transaction, session).unwrap();
        transaction.commit().unwrap();
    }
    assert_eq!(
        Job::load(&backend.connection, id).unwrap().state,
        State::Discarded
    );
    assert!(advance(&mut backend, id, State::Dispatched).is_err());
    backend.validate_maintenance_records().unwrap();
    assert_eq!(events(&backend, id), 3);
    let payload: String = backend.connection.query_row("SELECT result_payload FROM compaction_maintenance_events WHERE job_id = ?1 AND state = 3", [&id[..]], |row| row.get(0)).unwrap();
    assert_eq!(payload, result("READY_SUMMARY"));
}

#[tokio::test(flavor = "current_thread")]
async fn session_and_global_capacity_and_prefix_uniqueness_are_independent() {
    let (root, selected, store, session) = fixture("maintenance-capacity").await;
    let other = store
        .create_session_at(
            MutationRequestId::from_bytes([0xe2; 16]),
            None,
            selected.path().to_string_lossy().into_owned(),
        )
        .await
        .unwrap()
        .id;
    let mut runs = Vec::new();
    for (session, offset) in [(session, 0), (other, 10)] {
        for index in 1..=5 {
            let run = append_stopped(&store, session, index + offset, "TEXT".to_owned()).await;
            if index == 5 {
                runs.push(run);
            }
        }
    }
    drop(store);
    let mut backend = Backend::open(root.path()).unwrap();
    let first = seed(&mut backend, runs[0], 3).unwrap();
    assert!(seed(&mut backend, runs[0], 2).is_err());
    let second = seed(&mut backend, runs[1], 3).unwrap();
    advance(&mut backend, first, State::Dispatched).unwrap();
    assert!(advance(&mut backend, second, State::Dispatched).is_err());
    assert_eq!(events(&backend, second), 0);
    advance(&mut backend, first, State::Uncertain).unwrap();
    assert!(seed(&mut backend, runs[0], 3).is_err());
    advance(&mut backend, second, State::Dispatched).unwrap();
    backend.validate_maintenance_records().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn valid_stale_readiness_is_discarded_without_losing_result_or_usage() {
    for change in ["profile", "credential", "parent"] {
        let (root, _selected, mut backend, session, run) = populated().await;
        let id = seed(&mut backend, run, 3).unwrap();
        advance(&mut backend, id, State::Dispatched).unwrap();
        advance(&mut backend, id, State::Ready).unwrap();
        if change == "profile" {
            let mut job = Job::load(&backend.connection, id).unwrap();
            job.instruction_digest = [0x44; 32];
            backend.connection.execute("UPDATE compaction_maintenance_jobs SET instruction_digest = ?1, binding_digest = ?2 WHERE job_id = ?3", params![&job.instruction_digest[..], &job.digest()[..], &id[..]]).unwrap();
        } else if change == "parent" {
            let digest = backend.context_digest_through(session, 1).unwrap();
            let transaction = backend
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            let sequence = next_sequence(&transaction).unwrap();
            transaction.execute("INSERT INTO context_checkpoints VALUES (?1, ?2, NULL, 1, ?3, 4, 1, 'muse-spark-1.2', 'PARENT', 22, ?4, 1)", params![&[0x44_u8; 16][..], &session.as_bytes()[..], &digest[..], sequence_to_sql(sequence).unwrap()]).unwrap();
            transaction.commit().unwrap();
        }
        drop(backend);
        if change == "credential" {
            let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
            store
                .set_open_code_credential(
                    MutationRequestId::from_bytes([0xe3; 16]),
                    1,
                    b"different-fake-maintenance-key".to_vec(),
                )
                .await
                .unwrap();
            drop(store);
        }
        let backend = Backend::open(root.path()).unwrap();
        assert_eq!(
            Job::load(&backend.connection, id).unwrap().state,
            State::Discarded
        );
        assert_eq!(events(&backend, id), 3);
        let payload: String = backend.connection.query_row("SELECT result_payload FROM compaction_maintenance_events WHERE job_id = ?1 AND state = 3", [&id[..]], |row| row.get(0)).unwrap();
        assert_eq!(payload, result("READY_SUMMARY"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn corrupted_bindings_event_chains_and_results_fail_closed_before_recovery() {
    for sql in [
        "UPDATE compaction_maintenance_jobs SET source_digest = zeroblob(32)",
        "UPDATE compaction_maintenance_jobs SET binding_digest = zeroblob(32)",
        "UPDATE compaction_maintenance_jobs SET instruction_digest = zeroblob(32)",
        "UPDATE compaction_maintenance_jobs SET trigger_run_id = (SELECT run_id FROM run_accepted_facts ORDER BY fact_sequence LIMIT 1)",
        "UPDATE compaction_maintenance_jobs SET state = 7",
        "UPDATE compaction_maintenance_events SET result_digest = zeroblob(32) WHERE state = 3",
        "UPDATE compaction_maintenance_events SET result_payload = '{}' WHERE state = 3",
        "DELETE FROM compaction_maintenance_events WHERE state = 2",
    ] {
        let (root, _selected, mut backend, _session, run) = populated().await;
        let id = seed(&mut backend, run, 3).unwrap();
        advance(&mut backend, id, State::Dispatched).unwrap();
        advance(&mut backend, id, State::Ready).unwrap();
        backend.connection.execute_batch(sql).unwrap();
        drop(backend);
        assert!(Backend::open(root.path()).is_err(), "{sql}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ready_payload_bounds_and_usage_are_checked_before_committing() {
    let (_root, _selected, mut backend, _session, run) = populated().await;
    let id = seed(&mut backend, run, 3).unwrap();
    advance(&mut backend, id, State::Dispatched).unwrap();
    let job = Job::load(&backend.connection, id).unwrap();
    let baseline: serde_json::Value = serde_json::from_str(&result("summary")).unwrap();
    let mut invalid_results = vec![
        result(" "),
        result(&"x".repeat(16 * 1024 + 1)),
        result("NUL\0"),
        "{\"summary\":\"first\",\"summary\":\"second\"}".to_owned(),
    ];
    for (key, value) in [
        ("cached_input_tokens", serde_json::json!(101)),
        ("total_tokens", serde_json::json!(0)),
        ("output_tokens", serde_json::json!(4097)),
        ("reasoning_output_tokens", serde_json::json!(21)),
        ("unexpected", serde_json::json!(true)),
    ] {
        let mut payload = baseline.clone();
        payload[key] = value;
        invalid_results.push(payload.to_string());
    }
    for payload in invalid_results {
        let transaction = backend
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(transition(&transaction, &job, State::Ready, Some(&payload)).is_err());
    }
    assert_eq!(events(&backend, id), 1);
    assert_eq!(
        Job::load(&backend.connection, id).unwrap().state,
        State::Dispatched
    );
    advance(&mut backend, id, State::Ready).unwrap();
    backend.validate_maintenance_records().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn foreground_cannot_repeat_maintenance_prefix_without_explicit_manual_input() {
    let (root, _selected, mut backend, session, run) = populated().await;
    let triggering = load_required_run(&backend.connection, run).unwrap();
    let mut budget = backend.context_budget(session, 0, 5).unwrap();
    budget.observed_input_tokens = Some(70_000);
    let mut plan = backend
        .plan_context_compaction(&triggering, None, 5, 0, &budget)
        .unwrap()
        .unwrap();
    assert_eq!(plan.source_entry_high_water, 2);
    let id = seed(&mut backend, run, 2).unwrap();
    assert!(
        backend
            .plan_context_compaction(&triggering, None, 5, 0, &budget)
            .unwrap()
            .is_none()
    );
    advance(&mut backend, id, State::Dispatched).unwrap();
    drop(backend);
    let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
    for (request, text, allowed) in [
        (6, "continue", false),
        (7, "/compact preserve the goal", true),
    ] {
        let accepted = store
            .accept_session_input(
                MutationRequestId::from_bytes([request; 16]),
                session,
                text.to_owned(),
                RunModelSelection {
                    service: RunService::Zen,
                    model_id: "muse-spark-1.2".to_owned(),
                    protocol_revision: 1,
                    maximum_input_tokens: 96_000,
                    maximum_output_tokens: 32_000,
                    supports_tool_calls: true,
                    supports_image_input: false,
                },
            )
            .await
            .unwrap();
        store.activate_run(accepted.run.id).await.unwrap();
        if allowed {
            plan.user_guidance = Some("preserve the goal".to_owned());
        }
        let prepared = store.prepare_auto_compaction(accepted.run.id, &plan).await;
        assert_eq!(prepared.is_ok(), allowed);
        if let Ok(operation) = prepared {
            store
                .fail_compaction(accepted.run.id, operation, false)
                .await
                .unwrap();
        }
        store
            .finish_run_stopped(accepted.run.id, None)
            .await
            .unwrap();
    }
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    assert_eq!(
        Job::load(&backend.connection, id).unwrap().state,
        State::Uncertain
    );
    assert!(backend.compaction_prefix_was_attempted(session, 2).unwrap());
    assert!(!backend.compaction_prefix_was_attempted(session, 3).unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn data_use_policy_blocks_prepared_foreground_compaction_without_a_checkpoint() {
    let (root, _selected, backend, session, _run) = populated().await;
    drop(backend);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0xe4; 16]),
            session,
            "/compact".into(),
            RunModelSelection {
                service: RunService::Go,
                model_id: "gpt-5.6-luna".into(),
                protocol_revision: 1,
                maximum_input_tokens: 96_000,
                maximum_output_tokens: 32_000,
                supports_tool_calls: true,
                supports_image_input: true,
            },
        )
        .await
        .unwrap();
    store.activate_run(accepted.run.id).await.unwrap();
    let context = store.load_run_context(accepted.run.id).await.unwrap();
    let plan = context.compaction_plan.unwrap();
    let operation = store
        .prepare_auto_compaction(accepted.run.id, &plan)
        .await
        .unwrap();
    store
        .set_data_use_policy(
            MutationRequestId::from_bytes([0xe5; 16]),
            0,
            crate::provider::DataUseRestrictions {
                block_training_use: false,
                require_zero_retention: true,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .mark_compaction_dispatched(accepted.run.id, operation)
            .await,
        Err(PersistenceError::DataUseRestricted)
    ));
    store
        .fail_compaction(accepted.run.id, operation, false)
        .await
        .unwrap();
    store
        .finish_run_failure(
            accepted.run.id,
            None,
            crate::persistence::RunFailureKind::DataUseRestricted,
            crate::persistence::ProviderOperationFailureState::Failed,
        )
        .await
        .unwrap();
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    assert_eq!(
        backend
            .connection
            .query_row("SELECT state FROM compaction_operations", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        4
    );
    assert_eq!(
        backend
            .connection
            .query_row("SELECT COUNT(*) FROM context_checkpoints", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[tokio::test(flavor = "current_thread")]
async fn archival_discards_readiness_and_deletion_removes_only_owned_maintenance_state() {
    let (root, selected, mut backend, session, run) = populated().await;
    std::fs::write(selected.path().join("keep.txt"), "KEEP").unwrap();
    let id = seed(&mut backend, run, 3).unwrap();
    advance(&mut backend, id, State::Dispatched).unwrap();
    advance(&mut backend, id, State::Ready).unwrap();
    drop(backend);
    let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
    store
        .set_session_archived(MutationRequestId::from_bytes([0xe2; 16]), session, true)
        .await
        .unwrap();
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    assert_eq!(
        Job::load(&backend.connection, id).unwrap().state,
        State::Discarded
    );
    drop(backend);
    let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
    store
        .delete_session(MutationRequestId::from_bytes([0xe3; 16]), session)
        .await
        .unwrap();
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    let count: i64 = backend.connection.query_row("SELECT (SELECT COUNT(*) FROM compaction_maintenance_jobs) + (SELECT COUNT(*) FROM compaction_maintenance_events)", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        std::fs::read_to_string(selected.path().join("keep.txt")).unwrap(),
        "KEEP"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn preparation_requires_the_actual_idle_window_not_a_stale_trigger() {
    let (_root, _selected, mut backend, session, run) = populated().await;
    let id = seed(&mut backend, run, 3).unwrap();
    let previous = backend
        .connection
        .query_row(
            "SELECT run_id FROM session_entries WHERE session_id = ?1 AND entry_sequence = 4",
            [&session.as_bytes()[..]],
            |row| row.get::<_, [u8; 16]>(0),
        )
        .unwrap();
    let mut stale = Job::load(&backend.connection, id).unwrap();
    stale.run = load_required_run(&backend.connection, RunId::from_bytes(previous)).unwrap();
    stale.through = 4;
    stale.binding_digest = stale.digest();
    assert!(backend.validate_maintenance_binding(&stale).is_err());
    advance(&mut backend, id, State::Cancelled).unwrap();
    let command = "never dispatched";
    backend
        .accept_local_command(
            MutationRequestId::from_bytes([0xe2; 16]),
            super::super::local_command::local_command_fingerprint(session, command, false),
            session,
            command.to_owned(),
            false,
        )
        .unwrap();
    assert!(seed(&mut backend, run, 2).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn maintenance_sequences_cannot_reuse_unrelated_session_mutation_facts() {
    let (root, _selected, mut backend, session, run) = populated().await;
    let id = seed(&mut backend, run, 3).unwrap();
    advance(&mut backend, id, State::Dispatched).unwrap();
    advance(&mut backend, id, State::Ready).unwrap();
    drop(backend);
    let store = SessionStore::open_for_test_with_maintenance(root.path()).unwrap();
    store
        .rename_session(
            MutationRequestId::from_bytes([0xe2; 16]),
            session,
            "Renamed".to_owned(),
        )
        .await
        .unwrap();
    drop(store);
    let backend = Backend::open(root.path()).unwrap();
    backend.connection.execute("UPDATE compaction_maintenance_events SET fact_sequence = (SELECT accepted_sequence FROM session_rename_requests) WHERE job_id = ?1 AND state = 3", [&id[..]]).unwrap();
    assert!(backend.validate_maintenance_records().is_err());
    drop(backend);
    assert!(Backend::open(root.path()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn schema_26_migrates_without_scheduling_historical_sessions() {
    let (root, _selected, backend, session, _run) = populated().await;
    crate::persistence::data_use::tests::restore_schema_28(&backend.connection);
    backend
        .connection
        .execute_batch(
            "DROP TABLE compaction_maintenance_events; DROP TABLE compaction_maintenance_jobs;
        DROP INDEX compaction_operations_by_prefix; PRAGMA user_version = 26;",
        )
        .unwrap();
    drop(backend);
    let backend = Backend::open(root.path()).unwrap();
    let version: i64 = backend
        .connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 30);
    let jobs: i64 = backend
        .connection
        .query_row(
            "SELECT COUNT(*) FROM compaction_maintenance_jobs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(jobs, 0);
    let entries: i64 = backend
        .connection
        .query_row(
            "SELECT COUNT(*) FROM session_entries WHERE session_id = ?1",
            [&session.as_bytes()[..]],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(entries, 5);
}
