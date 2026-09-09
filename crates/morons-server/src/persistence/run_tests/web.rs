use super::*;
use crate::{
    persistence::{CommittedToolCall, RunId},
    provider::DataUseRestrictions,
    tools::{SubagentTask, ToolErrorKind, ToolInput, ValidatedProviderCall, WebSearchToolExecutor},
};
use std::sync::Arc;

fn tokens() -> crate::provider::openai_auth::OAuthTokens {
    crate::provider::openai_auth::OAuthTokens::fixture(
        "web-fixture-account",
        "synthetic-web-refresh",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600,
    )
}
async fn install(store: &SessionStore, generation: u64) {
    store
        .set_openai_credential(
            MutationRequestId::from_bytes([0xf0 + u8::try_from(generation).unwrap(); 16]),
            generation,
            tokens(),
        )
        .await
        .unwrap();
}
async fn prepare(store: &SessionStore, native: bool, task: bool) -> (RunId, CommittedToolCall) {
    configure_credential(store).await;
    if task {
        store
            .set_subagent_model_setting(
                MutationRequestId::from_bytes([0x91; 16]),
                SubagentModelSetting::Explicit {
                    service: RunService::Go,
                    model_id: "glm-5.3-flash".into(),
                },
            )
            .await
            .unwrap();
    }
    let session = store
        .create_session(MutationRequestId::from_bytes([0x92; 16]), None)
        .await
        .unwrap();
    let mut model = model_selection();
    if native {
        model.service = RunService::OpenAiChatGpt;
        model.model_id = "gpt-6-astra".into();
        model.protocol_revision = 5;
    }
    let accepted = store
        .accept_session_input(
            MutationRequestId::from_bytes([0x93; 16]),
            session.id,
            "web fixture".into(),
            model,
        )
        .await
        .unwrap();
    let run = accepted.run.id;
    store.activate_run(run).await.unwrap();
    let context = store.load_run_context(run).await.unwrap();
    let PrepareOperationOutcome::Prepared(operation) = store
        .prepare_provider_operation(
            run,
            context.current_entry_high_water,
            context.estimated_input_tokens,
        )
        .await
        .unwrap()
    else {
        panic!("prepared")
    };
    store
        .mark_provider_dispatched(run, operation)
        .await
        .unwrap();
    let input = if task {
        ToolInput::Task {
            context: "independent child".into(),
            tasks: vec![SubagentTask {
                name: None,
                task: "research".into(),
            }],
        }
    } else {
        ToolInput::WebSearch {
            query: "public query".into(),
        }
    };
    let mut turn = store
        .complete_provider_tool_turn(
            run,
            operation,
            CompletedToolTurn {
                provider_response_id: "resp_web_fixture".into(),
                usage: ProviderUsage {
                    input_tokens: 10,
                    cached_input_tokens: 0,
                    cache_write_input_tokens: 0,
                    output_tokens: 2,
                    reasoning_output_tokens: 0,
                    total_tokens: 12,
                },
                commentary: None,
                calls: vec![ValidatedProviderCall {
                    provider_call_id: "call_web_fixture".into(),
                    input,
                    opaque_continuation: None,
                }],
            },
        )
        .await
        .unwrap();
    let call = turn.calls.remove(0);
    store
        .prepare_tool_operation(run, call.call_id, call.operation_id, None)
        .await
        .unwrap();
    (run, call)
}

#[tokio::test(flavor = "current_thread")]
async fn web_bindings_pin_native_and_cross_provider_generations_policy_source_and_recovery() {
    for native in [false, true] {
        let root = TestRoot::new("web-binding");
        let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
        install(&store, 0).await;
        let (run, call) = prepare(&store, native, false).await;
        install(&store, 1).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        assert_eq!(binding.generation, if native { 1 } else { 2 });
        assert!(binding.query_digest.is_some());
        assert_eq!(binding.children, 0);
        let executor =
            WebSearchToolExecutor::for_test(store.clone(), "http://127.0.0.1:9/search".into());
        let (_, cancel) = crate::provider::provider_cancellation();
        assert!(
            executor
                .execute(
                    &ToolInput::WebSearch {
                        query: "foreign query".into()
                    },
                    &binding,
                    0,
                    0,
                    &cancel
                )
                .await
                .is_err()
        );
        assert!(
            executor
                .execute(&call.input, &binding, 1, 1, &cancel)
                .await
                .is_err()
        );
        let policy = store
            .set_data_use_policy(
                MutationRequestId::from_bytes([0x94; 16]),
                0,
                DataUseRestrictions {
                    block_training_use: true,
                    require_zero_retention: false,
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            store.admit_web_binding(&binding).await,
            Err(PersistenceError::DataUseRestricted)
        ));
        store
            .set_data_use_policy(
                MutationRequestId::from_bytes([0x95; 16]),
                policy.sequence,
                DataUseRestrictions::default(),
            )
            .await
            .unwrap();
        install(&store, 2).await;
        assert_eq!(
            executor
                .execute(&call.input, &binding, 0, 0, &cancel)
                .await
                .unwrap()
                .error_kind(),
            Some(ToolErrorKind::WebSearchUnavailable)
        );
        assert_eq!(store.web_binding(run, call.call_id).await.unwrap(), binding);
        assert!(
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .is_err()
        );
        drop(executor);
        drop(store);
        let store = SessionStore::open_for_test(root.path()).unwrap();
        assert!(store.admit_web_binding(&binding).await.is_err());
        assert_eq!(store.web_binding(run, call.call_id).await.unwrap(), binding);
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let uncertain: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM tool_operation_facts WHERE fact_kind=6",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(uncertain, 1);
        db.execute(
            "UPDATE web_model_bindings SET binding_digest=zeroblob(32)",
            [],
        )
        .unwrap();
        assert!(store.web_binding(run, call.call_id).await.is_err());
        drop(store);
        assert!(SessionStore::open_for_test(root.path()).is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn task_web_binding_does_not_borrow_go_generation_or_adopt_later_login() {
    for native in [false, true] {
        let root = TestRoot::new("child-web-binding");
        let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
        if native {
            install(&store, 0).await;
            install(&store, 1).await;
        }
        let (run, call) = prepare(&store, native, true).await;
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await
            .unwrap();
        let task = store.task_model_binding(run, call.call_id).await.unwrap();
        assert_eq!(task.service, RunService::Go);
        assert_eq!(task.credential_generation, 1);
        let binding = store.web_binding(run, call.call_id).await.unwrap();
        assert_eq!(binding.generation, if native { 2 } else { 0 });
        assert_eq!(binding.children, 1);
        assert!(binding.query_digest.is_none());
        if !native {
            install(&store, 0).await;
            assert!(matches!(
                store.admit_web_binding(&binding).await,
                Err(PersistenceError::OpenAiCredentialNotConfigured)
            ));
        }
        let executor =
            WebSearchToolExecutor::for_test(store.clone(), "http://127.0.0.1:9/search".into());
        let (_, cancel) = crate::provider::provider_cancellation();
        assert!(
            executor
                .execute(
                    &ToolInput::WebSearch {
                        query: "child query".into()
                    },
                    &binding,
                    2,
                    1,
                    &cancel
                )
                .await
                .is_err()
        );
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        db.execute("DELETE FROM web_model_bindings", []).unwrap();
        assert!(store.web_binding(run, call.call_id).await.is_err());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn schema_31_web_migration_preserves_canonical_input_and_credentials_without_invented_bindings()
 {
    let root = TestRoot::new("web-schema31");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    configure_credential(&store).await;
    install(&store, 0).await;
    let session = store
        .create_session(MutationRequestId::from_bytes([0xa1; 16]), None)
        .await
        .unwrap();
    let mut model = model_selection();
    model.supports_tool_calls = false;
    store
        .accept_session_input(
            MutationRequestId::from_bytes([0xa2; 16]),
            session.id,
            "Historical migration input".into(),
            model,
        )
        .await
        .unwrap();
    drop(store);
    let file = root.path().join("data/sessions.sqlite3");
    let db = Connection::open(&file).unwrap();
    let before: String = db
        .query_row(
            "SELECT text FROM session_entries WHERE entry_kind=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE; DROP TABLE web_model_bindings; DROP TABLE web_binding_epoch; PRAGMA user_version=31; COMMIT;").unwrap();
    drop(db);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store
            .open_code_credential_status()
            .await
            .unwrap()
            .generation,
        1
    );
    assert_eq!(
        store.openai_credential_status().await.unwrap().generation,
        1
    );
    let db = Connection::open(&file).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        32
    );
    assert_eq!(
        db.query_row(
            "SELECT text FROM session_entries WHERE entry_kind=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        before
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM web_model_bindings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        root.path()
            .join("backups/sessions-before-schema-v31.sqlite3")
            .exists()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn root_search_without_native_login_never_marks_dispatched_or_uses_brave() {
    let root = TestRoot::new("web-missing-login");
    let store = SessionStore::open_for_test(root.path()).unwrap();
    let (run, call) = prepare(&store, false, false).await;
    assert!(matches!(
        store
            .mark_tool_dispatched(run, call.call_id, call.operation_id)
            .await,
        Err(PersistenceError::OpenAiCredentialNotConfigured)
    ));
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM tool_operation_facts WHERE fact_kind=2",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert!(!include_str!("../../tools/web_search.rs").contains("BRAVE_SEARCH_API_KEY"));
    assert!(!include_str!("../../tools/web_search.rs").contains("search.brave.com"));
}
