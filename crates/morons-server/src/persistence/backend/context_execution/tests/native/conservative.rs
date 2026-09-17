use super::*;

mod boundaries;

fn counters(backend: &Backend, run: &Run) -> (u64, u64, u64, u64) {
    let run = load_required_run(&backend.connection, run.id).unwrap();
    (
        run.provider_turns,
        run.tool_calls,
        run.tool_mutations,
        run.tool_result_bytes,
    )
}

fn model() -> RunModelSelection {
    RunModelSelection {
        service: RunService::Zen,
        model_id: "muse-spark-1.2".into(),
        protocol_revision: 1,
        supports_image_input: false,
        ..selection()
    }
}

async fn conservative_fixture() -> Fixture {
    let (root, selected, store, session) =
        crate::persistence::backend::context_compaction::tests::fixture("repeated-parent").await;
    drop(store);
    Fixture {
        backend: Backend::open(root.path()).unwrap(),
        root,
        _selected: selected,
        session,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn conservative_parent_repeated_cuts_preserve_intent_counters_and_history() {
    let mut f = conservative_fixture().await;
    let run = f.accept_model(1, "Keep this exact user intent", model());
    assert_eq!(
        policy(&f.backend.connection, run.id).unwrap(),
        ExecutionPolicy::ConservativeRepeated
    );
    let mut previous_cut = 0;
    for attempt in 0..MAX_COMPACTIONS {
        let mut selected_plan = None;
        for turn in 0..=4 {
            let context = f.backend.load_run_context(run.id).unwrap();
            if let Some(plan) = context.compaction_plan {
                selected_plan = Some(plan);
                break;
            }
            if turn < 4 {
                f.read_turn(&run, 1000, 30_000);
            }
        }
        let plan = selected_plan.expect("four completed reads must produce a compaction cut");
        assert!(plan.source_entry_high_water > previous_cut);
        previous_cut = plan.source_entry_high_water;
        let before = rows(&f.backend.connection, "session_entries");
        let provider_before = rows(&f.backend.connection, "provider_operation_facts");
        let counters_before = counters(&f.backend, &run);
        let operation = f.backend.prepare_auto_compaction(run.id, &plan).unwrap();
        assert!(!can_compact(&f.backend.connection, run.id).unwrap());
        f.backend
            .mark_compaction_dispatched(run.id, operation)
            .unwrap();
        f.backend
            .complete_compaction(
                run.id,
                operation,
                RunService::Zen,
                "muse-spark-1.2",
                format!("Summary {attempt}: preserve intent and completed reads"),
            )
            .unwrap();
        assert_eq!(counters_before, counters(&f.backend, &run));
        assert_eq!(before, rows(&f.backend.connection, "session_entries"));
        assert_eq!(
            provider_before,
            rows(&f.backend.connection, "provider_operation_facts")
        );
        assert!(!can_compact(&f.backend.connection, run.id).unwrap());
        assert_eq!(
            f.backend.prepare_auto_compaction(run.id, &plan).unwrap(),
            operation
        );
        assert!(
            f.backend
                .mark_compaction_dispatched(run.id, operation)
                .is_err()
        );
        let context = f.backend.load_run_context(run.id).unwrap();
        assert!(context.compaction_plan.is_none());
        assert!(
            matches!(&context.entries[0], crate::persistence::TranscriptEntry::UserMessage { text, .. } if text == "Keep this exact user intent")
        );
        assert!(context.entries.len() >= 3);
        let status = f
            .backend
            .session_context_status(f.session, &model())
            .unwrap();
        assert!(!status.usage_admission);
    }
    f.read_turn(&run, 1000, 100);
    assert!(!can_compact(&f.backend.connection, run.id).unwrap());
    f.backend.finish_run_stopped(run.id, None).unwrap();
    drop(f.backend);
    drop(Backend::open(f.root.path()).unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn schema39_migration_keeps_old_parent_policy_and_rejects_corrupt_epoch() {
    let mut f = conservative_fixture().await;
    let old = f.accept_model(1, "old intent", model());
    f.backend.finish_run_stopped(old.id, None).unwrap();
    let before = rows(&f.backend.connection, "run_accepted_facts");
    crate::persistence::data_use::tests::restore_schema_40(&f.backend.connection);
    f.backend.connection.execute_batch("ALTER TABLE context_accounting_epoch DROP COLUMN repeated_first_sequence; PRAGMA user_version = 39;").unwrap();
    drop(f.backend);
    f.backend = Backend::open(f.root.path()).unwrap();
    assert_eq!(before, rows(&f.backend.connection, "run_accepted_facts"));
    assert_eq!(
        policy(&f.backend.connection, old.id).unwrap(),
        ExecutionPolicy::Legacy
    );
    let new = f.accept_model(2, "new intent", model());
    assert_eq!(
        policy(&f.backend.connection, new.id).unwrap(),
        ExecutionPolicy::ConservativeRepeated
    );
    f.backend.finish_run_stopped(new.id, None).unwrap();
    f.backend.connection.execute("UPDATE context_accounting_epoch SET repeated_first_sequence = (SELECT next_value + 1 FROM logical_sequences)", []).unwrap();
    assert!(validate_epoch(&f.backend.connection).is_err());
    drop(f.backend);
    assert!(Backend::open(f.root.path()).is_err());
}
