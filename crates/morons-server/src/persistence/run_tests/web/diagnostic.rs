use super::*;
use crate::{
    tools::ToolResult,
    web_diagnostic::{WebCategory, WebFailure, WebStage},
};
use tokio::{
    io::AsyncWriteExt as _,
    net::TcpListener,
    time::{self, Duration},
};

fn failure() -> ToolResult {
    ToolResult::error(ToolErrorKind::WebSearchUncertain(WebFailure {
        stage: WebStage::HttpStatus,
        category: WebCategory::RequestRejected,
    }))
}

#[tokio::test(flavor = "current_thread")]
async fn root_web_diagnostic_commits_uncertainty_and_survives_reopen_without_replay() {
    let root = TestRoot::new("web-diagnostic-root");
    let store = Arc::new(SessionStore::open_for_test(root.path()).unwrap());
    install(&store, 0).await;
    let (run, call) = prepare(&store, true, false).await;
    let session = store.load_run_context(run).await.unwrap().run.session_id;
    store
        .mark_tool_dispatched(run, call.call_id, call.operation_id)
        .await
        .unwrap();
    let binding = store.web_binding(run, call.call_id).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let executor = WebSearchToolExecutor::for_test(
        store.clone(),
        format!("http://{}/search", listener.local_addr().unwrap()),
    );
    let peer = tokio::spawn(async move {
        let (mut stream, _) = time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        time::timeout(
            Duration::from_secs(10),
            crate::provider::openai_auth::tests::mock_request(&mut stream),
        )
        .await
        .unwrap();
        stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 7\r\nConnection: close\r\n\r\nPRIVATE").await.unwrap();
        listener
    });
    let (_, cancel) = crate::provider::provider_cancellation();
    let result = executor
        .execute(&call.input, &binding, 0, 0, &cancel)
        .await
        .unwrap();
    assert_eq!(result, failure());
    let listener = peer.await.unwrap();
    let entry = store
        .complete_tool_result(run, call.call_id, call.operation_id, result.clone())
        .await
        .unwrap();
    let TranscriptEntry::ToolResult {
        result: visible, ..
    } = entry
    else {
        panic!("tool result")
    };
    assert!(
        visible
            .summary()
            .contains("stage: http-status; category: request-rejected")
    );
    assert!(visible.summary().contains("nothing was retried"));
    assert!(!visible.summary().contains("PRIVATE"));
    assert_eq!(
        store.get_run(session, run).await.unwrap().unwrap().state,
        RunState::Uncertain
    );
    assert!(
        executor
            .execute(&call.input, &binding, 0, 0, &cancel)
            .await
            .unwrap()
            .error_kind()
            == Some(ToolErrorKind::Cancelled)
    );
    assert!(
        time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    drop(executor);
    drop(store);
    let store = SessionStore::open_for_test(root.path()).unwrap();
    assert_eq!(
        store.get_run(session, run).await.unwrap().unwrap().state,
        RunState::Uncertain
    );
    assert_eq!(store.web_binding(run, call.call_id).await.unwrap(), binding);
    let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    let payload: Vec<u8> = db
        .query_row(
            "SELECT result_payload FROM tool_operation_facts WHERE fact_kind=6",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<ToolResult>(&payload).unwrap(),
        result
    );
    // External canonical changes must not be hidden by a still-current derived run projection.
    db.execute(
        "UPDATE run_accepted_facts SET tool_catalog_version=11,tool_limits_version=11",
        [],
    )
    .unwrap();
    assert!(store.web_binding(run, call.call_id).await.is_err());
    db.execute(
        "UPDATE run_accepted_facts SET tool_catalog_version=12,tool_limits_version=12",
        [],
    )
    .unwrap();
    assert!(store.web_binding(run, call.call_id).await.is_ok());
    let mut corrupt = serde_json::from_slice::<serde_json::Value>(&payload).unwrap();
    corrupt["error"]["web_search_uncertain"]["raw_body"] = serde_json::json!("PRIVATE");
    db.execute(
        "UPDATE tool_operation_facts SET result_payload=?1 WHERE fact_kind=6",
        [serde_json::to_vec(&corrupt).unwrap()],
    )
    .unwrap();
    assert!(store.web_binding(run, call.call_id).await.is_err());
    drop(store);
    assert!(SessionStore::open_for_test(root.path()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn web_diagnostic_requires_current_catalog_dispatched_binding_and_native_generation() {
    for case in 0..3 {
        let root = TestRoot::new("web-diagnostic-scope");
        let store = SessionStore::open_for_test(root.path()).unwrap();
        if case != 2 {
            install(&store, 0).await;
        }
        let (run, call) = prepare(&store, case != 2, case == 2).await;
        if case != 0 {
            store
                .mark_tool_dispatched(run, call.call_id, call.operation_id)
                .await
                .unwrap();
        }
        if case == 1 {
            let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
            db.execute(
                "UPDATE run_accepted_facts SET tool_catalog_version=11,tool_limits_version=11",
                [],
            )
            .unwrap();
            db.execute(
                "UPDATE runs SET tool_catalog_version=11,tool_limits_version=11",
                [],
            )
            .unwrap();
            db.execute("UPDATE provider_operation_facts SET tool_catalog_version=11,tool_limits_version=11 WHERE fact_kind=1", []).unwrap();
        }
        assert!(
            store
                .complete_tool_result(run, call.call_id, call.operation_id, failure())
                .await
                .is_err()
        );
        drop(store);
        let store = SessionStore::open_for_test(root.path()).unwrap();
        let db = Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
        let bad:i64=db.query_row("SELECT COUNT(*) FROM tool_operation_facts WHERE instr(CAST(result_payload AS TEXT),'web_search_uncertain')>0",[],|r|r.get(0)).unwrap();
        assert_eq!(bad, 0);
        drop(store);
    }
}
