use super::super::super::{
    Backend, context_usage::ContextModel, project_context, run_queries::load_run_skills,
    run_records::load_required_run,
};
use super::*;
use crate::{
    persistence::{
        ActivationOutcome, CompletedToolTurn, DispatchOutcome, MutationRequestId,
        PersistenceResourceLimit, PrepareOperationOutcome, ProviderOperationFailureState,
        ProviderUsage, Run, RunFailureKind, RunInputContext, RunModelSelection, RunService,
        SessionId, SessionStore, run_types::ProviderOperationId, tests::TestRoot,
        types::submit_session_input_fingerprint,
    },
    tools::{ToolInput, ToolOutput, ToolPath, ToolResult, ValidatedProviderCall},
};

struct Fixture {
    backend: Backend,
    root: TestRoot,
    _selected: TestRoot,
    session: SessionId,
}

fn selection() -> RunModelSelection {
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

async fn fixture() -> Fixture {
    let root = TestRoot::new("native-provenance");
    let selected = TestRoot::new("native-provenance-directory");
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
            crate::provider::openai_auth::OAuthTokens::fixture(
                "synthetic-provenance",
                "synthetic-refresh",
                expires,
            ),
        )
        .await
        .unwrap();
    let session = store
        .create_session_at(
            MutationRequestId::from_bytes([0xe1; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap()
        .id;
    drop(store);
    Fixture {
        backend: Backend::open(root.path()).unwrap(),
        root,
        _selected: selected,
        session,
    }
}

impl Fixture {
    fn accept(&mut self, request: u8, text: &str) -> Run {
        let model = selection();
        let fingerprint =
            submit_session_input_fingerprint(self.session, text, model.service, &model.model_id);
        let run = self
            .backend
            .accept_session_input(
                MutationRequestId::from_bytes([request; 16]),
                fingerprint,
                self.session,
                text.into(),
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
            self.backend.activate_run(run.id).unwrap(),
            ActivationOutcome::Active
        );
        load_required_run(&self.backend.connection, run.id).unwrap()
    }

    fn prepare(&mut self, run: &Run) -> ProviderOperationId {
        let context = self.backend.load_run_context(run.id).unwrap();
        assert!(context.compaction_plan.is_none());
        let PrepareOperationOutcome::Prepared(operation) = self
            .backend
            .prepare_provider_operation(
                run.id,
                context.current_entry_high_water,
                context.estimated_input_tokens,
            )
            .unwrap()
        else {
            panic!("fixture operation must prepare")
        };
        operation
    }

    // Synthetic storage transitions only: no provider, filesystem read or executor.
    fn read_turn(&mut self, run: &Run, input_tokens: u64, bytes: usize) {
        let operation = self.prepare(run);
        assert_eq!(
            self.backend
                .mark_provider_dispatched(run.id, operation)
                .unwrap(),
            DispatchOutcome::Dispatched
        );
        let path = ToolPath::parse("synthetic.txt").unwrap();
        let turn = self
            .backend
            .complete_provider_tool_turn(
                run.id,
                operation,
                CompletedToolTurn {
                    provider_response_id: "synthetic-response".into(),
                    commentary: None,
                    usage: ProviderUsage {
                        input_tokens,
                        cached_input_tokens: input_tokens * 3 / 5,
                        cache_write_input_tokens: input_tokens / 10,
                        output_tokens: 3,
                        reasoning_output_tokens: 0,
                        total_tokens: input_tokens + 3,
                    },
                    calls: vec![ValidatedProviderCall {
                        provider_call_id: "synthetic-call".into(),
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
        self.backend
            .prepare_tool_operation(run.id, call.call_id, call.operation_id, None)
            .unwrap();
        self.backend
            .mark_tool_dispatched(run.id, call.call_id, call.operation_id)
            .unwrap();
        self.backend
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
                        text: "x".repeat(bytes),
                    },
                },
            )
            .unwrap();
    }

    fn heavy_run(&mut self, input_tokens: u64) -> Run {
        let run = self.accept(1, &"u".repeat(50_000));
        self.read_turn(&run, input_tokens, 48_000);
        run
    }

    fn assert_fallback(&self, run: &Run) {
        let before = rows(&self.backend.connection, "provider_operation_facts");
        let status = self
            .backend
            .session_context_status(self.session, &selection())
            .unwrap();
        assert!(status.usage_admission);
        assert!(!status.estimate_uses_provider_usage);
        assert!(status.latest_provider_usage.is_none());
        assert!(status.conservative_input_tokens > 96_000);
        assert_eq!(
            status.admission_input_tokens,
            status.conservative_input_tokens
        );
        assert!(matches!(
            self.backend.load_run_context(run.id),
            Err(PersistenceError::ResourceLimit {
                resource: PersistenceResourceLimit::Context,
            })
        ));
        assert_eq!(
            before,
            rows(&self.backend.connection, "provider_operation_facts")
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_epoch_migration_preserves_old_profile_and_selects_only_new_runs() {
    let mut f = fixture().await;
    let old = f.accept(1, "old native intent");
    f.read_turn(&old, 1000, 8);
    f.backend.finish_run_stopped(old.id, None).unwrap();
    assert_eq!(
        policy(&f.backend.connection, old.id).unwrap(),
        ExecutionPolicy::NativeUsage
    );
    let tables = [
        "run_accepted_facts",
        "run_state_facts",
        "session_entries",
        "provider_operation_facts",
        "tool_calls",
    ];
    let before = tables.map(|table| rows(&f.backend.connection, table));
    let Fixture {
        backend,
        root,
        _selected,
        session,
    } = f;
    drop(backend);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    crate::persistence::data_use::tests::restore_schema_32(&db);
    drop(db);
    let backend = Backend::open(root.path()).unwrap();
    assert_eq!(before, tables.map(|table| rows(&backend.connection, table)));
    assert_eq!(
        policy(&backend.connection, old.id).unwrap(),
        ExecutionPolicy::Legacy
    );
    assert!(
        !backend
            .session_context_status(session, &selection())
            .unwrap()
            .usage_admission
    );
    let backup = Connection::open(
        root.path()
            .join("backups/sessions-before-schema-v32.sqlite3"),
    )
    .unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        32
    );
    assert_eq!(before, tables.map(|table| rows(&backup, table)));
    drop(backup);
    let mut f = Fixture {
        backend,
        root,
        _selected,
        session,
    };
    let new = f.accept(2, "new native intent");
    assert_eq!(old.service, new.service);
    assert_eq!(old.model_id, new.model_id);
    assert_eq!(old.protocol_revision, new.protocol_revision);
    assert_eq!(old.credential_generation, new.credential_generation);
    assert_eq!(old.context_policy_version, new.context_policy_version);
    assert_eq!(old.tool_catalog_version, new.tool_catalog_version);
    assert_eq!(old.tool_limits_version, new.tool_limits_version);
    assert_eq!(
        (old.maximum_input_tokens, old.maximum_output_tokens),
        (new.maximum_input_tokens, new.maximum_output_tokens)
    );
    assert_eq!(
        policy(&f.backend.connection, old.id).unwrap(),
        ExecutionPolicy::Legacy
    );
    assert_eq!(
        policy(&f.backend.connection, new.id).unwrap(),
        ExecutionPolicy::NativeUsage
    );
    assert!(
        f.backend
            .session_context_status(session, &selection())
            .unwrap()
            .usage_admission
    );
    let crosses: bool = f.backend.connection.query_row(
        "SELECT old.fact_sequence < epoch.first_sequence AND new.fact_sequence >= epoch.first_sequence FROM run_accepted_facts AS old, run_accepted_facts AS new, context_accounting_epoch AS epoch WHERE old.run_id=?1 AND new.run_id=?2",
        params![&old.id.as_bytes()[..], &new.id.as_bytes()[..]], |r| r.get(0),
    ).unwrap();
    assert!(crosses);
    f.backend.finish_run_stopped(new.id, None).unwrap();
    drop(f.backend);
    let reopened = Backend::open(f.root.path()).unwrap();
    assert_eq!(
        policy(&reopened.connection, old.id).unwrap(),
        ExecutionPolicy::Legacy
    );
    assert_eq!(
        policy(&reopened.connection, new.id).unwrap(),
        ExecutionPolicy::NativeUsage
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_usage_scope_and_cache_once_match_metadata_and_projected_admission() {
    let mut f = fixture().await;
    let run = f.accept(1, &"u".repeat(50_000));
    let first = f
        .backend
        .session_context_status(f.session, &selection())
        .unwrap();
    assert!(!first.estimate_uses_provider_usage && first.latest_provider_usage.is_none());
    assert_eq!(
        first.admission_input_tokens,
        first.conservative_input_tokens
    );
    f.read_turn(&run, 1000, 48_000);
    let status = f
        .backend
        .session_context_status(f.session, &selection())
        .unwrap();
    assert!(status.conservative_input_tokens > 96_000);
    let tail = f.backend.context_budget(f.session, 1, 3).unwrap();
    assert_eq!(
        u64::from(status.admission_input_tokens),
        1000 + 3 + tail.tokens(0)
    );
    let usage = status.latest_provider_usage.unwrap();
    assert_eq!(
        (
            usage.input_tokens,
            usage.cached_input_tokens,
            usage.cache_write_input_tokens
        ),
        (1000, 600, 100)
    );
    let context = f.backend.load_run_context(run.id).unwrap();
    assert!(context.compaction_plan.is_none());
    assert_eq!(
        context.estimated_input_tokens,
        status.admission_input_tokens
    );
    let skills = load_run_skills(&f.backend.connection, run.id).unwrap();
    let project = project_context::load(&f.backend.connection, run.id).unwrap();
    for fault in 0..5 {
        let mut wrong = run.clone();
        match fault {
            0 => wrong.id = RunId::from_bytes([0xff; 16]),
            1 => wrong.credential_generation += 1,
            2 => wrong.service = RunService::Go,
            3 => wrong.model_id = "gpt-5.6-sol".into(),
            _ => wrong.protocol_revision += 1,
        }
        // Exercise the scope seam without corrupting canonical rows or credentials.
        assert!(
            f.backend
                .observe_run_usage(&wrong, None, 3, &skills, project.as_ref())
                .unwrap()
                .is_none()
        );
    }
    assert!(
        f.backend
            .observe_run_usage(&run, None, 3, &skills, project.as_ref())
            .unwrap()
            .is_some()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_latest_noncompleted_preparation_cannot_reuse_an_older_receipt() {
    #[derive(Clone, Copy)]
    enum Phase {
        Prepared,
        Dispatched,
        Failed,
        Uncertain,
        Abandoned,
    }
    for (phase, expected) in [
        (Phase::Prepared, &[1][..]),
        (Phase::Dispatched, &[1, 2][..]),
        (Phase::Failed, &[1, 4][..]),
        (Phase::Uncertain, &[1, 2, 5][..]),
        (Phase::Abandoned, &[1, 6][..]),
    ] {
        let mut f = fixture().await;
        let run = f.heavy_run(1000);
        let operation = f.prepare(&run);
        if matches!(phase, Phase::Dispatched | Phase::Uncertain) {
            f.backend
                .mark_provider_dispatched(run.id, operation)
                .unwrap();
        }
        match phase {
            Phase::Failed | Phase::Uncertain => {
                f.backend
                    .finish_run_failure(
                        run.id,
                        Some(operation),
                        RunFailureKind::ProviderUnavailable,
                        if matches!(phase, Phase::Failed) {
                            ProviderOperationFailureState::Failed
                        } else {
                            ProviderOperationFailureState::Uncertain
                        },
                    )
                    .unwrap();
            }
            Phase::Abandoned => {
                f.backend
                    .finish_run_stopped(run.id, Some(operation))
                    .unwrap();
            }
            Phase::Prepared | Phase::Dispatched => {}
        }
        let facts = f.backend.connection.prepare(
            "SELECT fact_kind FROM provider_operation_facts WHERE operation_id=?1 ORDER BY fact_sequence",
        ).unwrap().query_map([&operation.as_bytes()[..]], |r| r.get::<_, u32>(0)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap();
        assert_eq!(facts, expected);
        f.assert_fallback(&run);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_zero_input_receipt_falls_back_in_status_and_execution() {
    let mut f = fixture().await;
    let run = f.heavy_run(0);
    f.assert_fallback(&run);
}

#[tokio::test(flavor = "current_thread")]
async fn native_previous_run_receipt_requires_compaction_instead_of_borrowed_credit() {
    let mut f = fixture().await;
    let prior = f.heavy_run(1000);
    f.backend.finish_run_stopped(prior.id, None).unwrap();
    let run = f.accept(2, "continue");
    let skills = load_run_skills(&f.backend.connection, run.id).unwrap();
    let project = project_context::load(&f.backend.connection, run.id).unwrap();
    // The shared advisory lookup CAN see the prior receipt; native admission must not use it.
    let advisory = f
        .backend
        .observe_context_usage(
            f.session,
            ContextModel {
                service: run.service,
                model_id: &run.model_id,
                protocol_revision: run.protocol_revision,
            },
            None,
            4,
            &skills,
            project.as_ref(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(advisory.run_id, prior.id);
    assert!(advisory.estimated_tokens < 67_200);
    let status = f
        .backend
        .session_context_status(f.session, &selection())
        .unwrap();
    assert!(status.usage_admission && !status.estimate_uses_provider_usage);
    assert!(status.admission_input_tokens > 96_000);
    assert_eq!(
        status.admission_input_tokens,
        status.conservative_input_tokens
    );
    let context = f.backend.load_run_context(run.id).unwrap();
    let plan = context
        .compaction_plan
        .expect("native admission requires a summary, not prior-run credit");
    assert_eq!(plan.source_entry_high_water, 3);
    assert!(plan.source_entry_high_water < run.source_entry_high_water);
}
