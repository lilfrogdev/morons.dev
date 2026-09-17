use super::*;
use crate::persistence::backend::context_budget::MAX_COMPACTION_SUMMARY_BYTES;

#[tokio::test(flavor = "current_thread")]
async fn conservative_parent_policy_requires_exact_profile_and_epoch() {
    let mut f = conservative_fixture().await;
    let run = f.accept_model(1, "scope", model());
    for service in [1, 2] {
        for protocol in 1..=4 {
            f.backend
                .connection
                .execute_batch("SAVEPOINT eligible_scope")
                .unwrap();
            f.backend
                .connection
                .execute(
                    "UPDATE run_accepted_facts SET open_code_service = ?1, protocol_revision = ?2",
                    rusqlite::params![service, protocol],
                )
                .unwrap();
            f.backend
                .connection
                .execute(
                    "UPDATE context_accounting_epoch SET repeated_first_sequence = (SELECT fact_sequence FROM run_accepted_facts WHERE run_id = ?1)",
                    [&run.id.as_bytes()[..]],
                )
                .unwrap();
            assert_eq!(
                policy(&f.backend.connection, run.id).unwrap(),
                ExecutionPolicy::ConservativeRepeated,
                "service {service}, protocol {protocol}, at epoch"
            );
            f.backend
                .connection
                .execute_batch("UPDATE context_accounting_epoch SET repeated_first_sequence = repeated_first_sequence + 1")
                .unwrap();
            assert_eq!(
                policy(&f.backend.connection, run.id).unwrap(),
                ExecutionPolicy::Legacy,
                "service {service}, protocol {protocol}, before epoch"
            );
            f.backend
                .connection
                .execute_batch("ROLLBACK TO eligible_scope; RELEASE eligible_scope")
                .unwrap();
        }
    }
    for (column, excluded) in [
        ("open_code_service", 3),
        ("protocol_revision", 5),
        ("context_policy_version", 3),
        ("tool_catalog_version", 13),
        ("tool_limits_version", 13),
        ("maximum_input_tokens", 95_999),
        ("maximum_output_tokens", 31_999),
    ] {
        f.backend
            .connection
            .execute_batch("SAVEPOINT scope")
            .unwrap();
        f.backend
            .connection
            .execute(
                &format!("UPDATE run_accepted_facts SET {column} = ?1"),
                [excluded],
            )
            .unwrap();
        assert_eq!(
            policy(&f.backend.connection, run.id).unwrap(),
            ExecutionPolicy::Legacy,
            "{column}"
        );
        f.backend
            .connection
            .execute_batch("ROLLBACK TO scope; RELEASE scope")
            .unwrap();
    }
    f.backend.connection.execute("UPDATE context_accounting_epoch SET repeated_first_sequence = (SELECT next_value FROM logical_sequences)", []).unwrap();
    assert_eq!(
        policy(&f.backend.connection, run.id).unwrap(),
        ExecutionPolicy::Legacy
    );
}

#[tokio::test(flavor = "current_thread")]
async fn conservative_parent_reopen_rejects_invalid_compaction_epochs() {
    for corruption in [
        "DELETE FROM context_accounting_epoch",
        "UPDATE context_accounting_epoch SET repeated_first_sequence = 0",
        "UPDATE context_accounting_epoch SET first_sequence = 2, repeated_first_sequence = 1",
        "UPDATE context_accounting_epoch SET repeated_first_sequence = (SELECT next_value + 1 FROM logical_sequences WHERE singleton = 1)",
    ] {
        let mut f = conservative_fixture().await;
        let run = f.accept_model(1, "epoch validation", model());
        f.backend.finish_run_stopped(run.id, None).unwrap();
        // Bypass SQL checks to exercise validation of a damaged database on open.
        f.backend
            .connection
            .execute_batch("PRAGMA ignore_check_constraints = ON")
            .unwrap();
        f.backend.connection.execute_batch(corruption).unwrap();
        assert!(
            validate_epoch(&f.backend.connection).is_err(),
            "{corruption}"
        );
        drop(f.backend);
        assert!(Backend::open(f.root.path()).is_err(), "{corruption}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn conservative_parent_cut_guarantees_reduction_with_maximum_summary() {
    let mut f = conservative_fixture().await;
    let run = f.accept_model(1, "/compact", model());
    f.read_turn(&run, 1000, 100);
    f.read_turn(&run, 1000, 100);
    assert!(
        f.backend
            .load_run_context(run.id)
            .unwrap()
            .compaction_plan
            .is_none()
    );
    f.read_turn(&run, 1000, 30_000);
    f.read_turn(&run, 1000, 100);
    let context = f.backend.load_run_context(run.id).unwrap();
    let plan = context.compaction_plan.unwrap();
    let removed = f
        .backend
        .context_budget(f.session, 0, plan.source_entry_high_water)
        .unwrap();
    let user = f
        .backend
        .context_budget(
            f.session,
            run.source_entry_high_water - 1,
            run.source_entry_high_water,
        )
        .unwrap();
    assert!(removed.bytes > user.bytes + MAX_COMPACTION_SUMMARY_BYTES as u64);
    let operation = f.backend.prepare_auto_compaction(run.id, &plan).unwrap();
    f.backend
        .mark_compaction_dispatched(run.id, operation)
        .unwrap();
    for summary in [String::new(), "s".repeat(MAX_COMPACTION_SUMMARY_BYTES + 1)] {
        assert!(
            f.backend
                .complete_compaction(
                    run.id,
                    operation,
                    RunService::Zen,
                    "muse-spark-1.2",
                    summary
                )
                .is_err()
        );
        assert!(rows(&f.backend.connection, "context_checkpoints").is_empty());
    }
    f.backend
        .complete_compaction(
            run.id,
            operation,
            RunService::Zen,
            "muse-spark-1.2",
            "s".repeat(MAX_COMPACTION_SUMMARY_BYTES),
        )
        .unwrap();
    let after = f.backend.load_run_context(run.id).unwrap();
    assert!(after.estimated_input_tokens < context.estimated_input_tokens);
    assert!(after.compaction_plan.is_none());
    f.backend.finish_run_stopped(run.id, None).unwrap();
    drop(f.backend);
    drop(Backend::open(f.root.path()).unwrap());
}

#[tokio::test(flavor = "current_thread")]
async fn conservative_parent_failed_and_interrupted_compactions_never_replay() {
    for dispatched in [false, true] {
        for recover in [false, true] {
            let mut f = conservative_fixture().await;
            let run = f.accept_model(1, "/compact", model());
            f.read_turn(&run, 1000, 30_000);
            f.read_turn(&run, 1000, 100);
            let plan = f
                .backend
                .load_run_context(run.id)
                .unwrap()
                .compaction_plan
                .unwrap();
            let operation = f.backend.prepare_auto_compaction(run.id, &plan).unwrap();
            if dispatched {
                f.backend
                    .mark_compaction_dispatched(run.id, operation)
                    .unwrap();
            }
            let history = rows(&f.backend.connection, "session_entries");
            if recover {
                drop(f.backend);
                f.backend = Backend::open(f.root.path()).unwrap();
            } else {
                f.backend
                    .fail_compaction(run.id, operation, dispatched)
                    .unwrap();
            }
            let state: u32 = f
                .backend
                .connection
                .query_row("SELECT state FROM compaction_operations", [], |r| r.get(0))
                .unwrap();
            assert_eq!(state, if dispatched { 5 } else { 4 });
            assert!(!can_compact(&f.backend.connection, run.id).unwrap());
            assert!(
                f.backend
                    .mark_compaction_dispatched(run.id, operation)
                    .is_err()
            );
            assert!(
                f.backend
                    .complete_compaction(
                        run.id,
                        operation,
                        RunService::Zen,
                        "muse-spark-1.2",
                        "late result".into()
                    )
                    .is_err()
            );
            assert!(rows(&f.backend.connection, "context_checkpoints").is_empty());
            assert_eq!(history, rows(&f.backend.connection, "session_entries"));
            if !recover {
                f.backend.finish_run_stopped(run.id, None).unwrap();
            }
            drop(f.backend);
            drop(Backend::open(f.root.path()).unwrap());
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn conservative_parent_current_user_image_prohibits_within_run_cut() {
    use sha2::{Digest, Sha256};
    let mut f = conservative_fixture().await;
    let image = morons_image::normalize_rgba(2, 2, vec![0x44; 16]).unwrap();
    let attachment = crate::persistence::PreparedImageAttachment {
        display_name: "picture.png".into(),
        marker_start: 9,
        media_type: image.media_type,
        width: image.width,
        height: image.height,
        digest: Sha256::digest(&image.bytes).into(),
        bytes: image.bytes,
    };
    let text = "/compact [picture.png]";
    let run = f
        .backend
        .accept_session_input(
            MutationRequestId::from_bytes([1; 16]),
            submit_session_input_fingerprint(f.session, text, RunService::Zen, "muse-spark-1.2"),
            f.session,
            text.into(),
            RunModelSelection {
                supports_image_input: true,
                ..model()
            },
            RunInputContext {
                skills: Default::default(),
                project: Default::default(),
                attachments: vec![attachment],
            },
        )
        .unwrap()
        .run;
    assert_eq!(
        f.backend.activate_run(run.id).unwrap(),
        ActivationOutcome::Active
    );
    f.read_turn(&run, 1000, 30_000);
    f.read_turn(&run, 1000, 100);
    let cut: i64 = f
        .backend
        .connection
        .query_row(
            "SELECT MIN(entry_sequence) FROM session_entries WHERE entry_kind = 4",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        policy(&f.backend.connection, run.id).unwrap(),
        ExecutionPolicy::ConservativeRepeated
    );
    assert!(
        f.backend
            .load_run_context(run.id)
            .unwrap()
            .compaction_plan
            .is_none()
    );
    assert!(
        !crate::persistence::backend::context_compaction::within_run::source_allowed(
            &f.backend.connection,
            run.id,
            u64::try_from(cut).unwrap(),
            None
        )
        .unwrap()
    );
}
