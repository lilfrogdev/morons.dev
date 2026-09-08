use super::*;
mod admission;
mod compaction;
mod diagnostics;
mod lifecycle;
mod mixed;

const NATIVE_MODELS: [&str; 6] = [
    "gpt-5.5",
    "gpt-6-astra",
    "gpt-5.6-sol",
    "gpt-5.6-luna",
    "gpt-5.6-terra",
    "gpt-daybreak-blue-latest",
];

fn synthetic_tokens() -> crate::provider::openai_auth::OAuthTokens {
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    crate::provider::openai_auth::OAuthTokens::fixture(
        "native-application-account",
        "synthetic-refresh",
        expires,
    )
}
async fn write_native_model(
    stream: &mut tokio::net::TcpStream,
    id: &str,
    output: &str,
    model: &str,
) {
    let body = provider_output_body(id, output).replace("muse-spark-1.2", model);
    write_provider_headers(stream, body.len()).await;
    stream.write_all(body.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn native_root_executes_image_tools_and_receipt_bound_reasoning_without_opencode_credentials()
{
    for model in NATIVE_MODELS {
        root_image_tool_flow(model).await;
    }
}

async fn root_image_tool_flow(model: &'static str) {
    let root = TestRoot::new("native-root");
    let selected = TestRoot::new("native-selected");
    fs::write(selected.path().join("AGENTS.md"), "NATIVE_PROJECT_GUIDANCE").unwrap();
    let image = morons_image::normalize_rgba(2, 2, vec![0x55; 16]).unwrap();
    fs::write(selected.path().join("picture.png"), image.bytes).unwrap();
    let store = SessionStore::open_for_test(root.path()).unwrap();
    store
        .set_openai_credential(
            PersistenceMutationRequestId::from_bytes([0xe1; 16]),
            0,
            synthetic_tokens(),
        )
        .await
        .unwrap();
    assert!(
        !store
            .open_code_credential_status()
            .await
            .unwrap()
            .configured
    );
    let session = store
        .create_session_at(
            PersistenceMutationRequestId::from_bytes([0xe2; 16]),
            None,
            selected.path().to_str().unwrap().into(),
        )
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut session_header = None;
        for index in 0..2 {
            let (mut stream, _) = time::timeout(TERMINAL_RUN_TEST_TIMEOUT, listener.accept())
                .await
                .unwrap()
                .unwrap();
            let request = String::from_utf8(read_http_request(&mut stream).await).unwrap();
            assert!(request.starts_with("POST /backend-api/codex/responses"));
            let (headers, body) = request.split_once("\r\n\r\n").unwrap();
            assert!(!headers.contains("x-opencode-session"));
            assert_eq!(request_header(&request, "originator"), "morons");
            let current = request_header(&request, "session-id");
            if let Some(previous) = &session_header {
                assert_eq!(&current, previous);
            } else {
                session_header = Some(current);
            }
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(body["model"], model);
            assert_eq!(body["store"], false);
            assert!(body.get("max_output_tokens").is_none());
            assert!(
                body["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("Selected working directory")
            );
            assert!(
                !body["instructions"]
                    .as_str()
                    .unwrap()
                    .contains("NATIVE_PROJECT_GUIDANCE")
            );
            assert!(
                body["input"]
                    .to_string()
                    .contains("NATIVE_PROJECT_GUIDANCE")
            );
            if index == 0 {
                let arguments = serde_json::json!({"path":"picture.png"}).to_string();
                let output = format!(
                    "{},{}",
                    serde_json::json!({"id":"rs_native","type":"reasoning","summary":[{"type":"summary_text","text":"bounded summary"}],"encrypted_content":"synthetic-native-continuation"}),
                    serde_json::json!({"id":"fc_native","type":"function_call","status":"completed","call_id":"native_read","name":"read","arguments":arguments})
                );
                // Native may omit the media type; receipt-bound continuation
                // must still complete through the same strict decoder (ADR0037).
                let response = provider_output_body("resp_native_read", &output)
                    .replace("muse-spark-1.2", model);
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            response.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            } else {
                assert!(body["input"].to_string().contains("data:image/png;base64,"));
                assert!(
                    body["input"]
                        .to_string()
                        .contains("synthetic-native-continuation")
                );
                write_native_model(&mut stream,"resp_native_final",r#"{"id":"msg_native","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Image inspected.","annotations":[]}]}"#, model).await;
            }
        }
    });
    let app = ServerApplication::from_native_store_for_test(store, &base);
    let session_id = SessionId::from_bytes(*session.id.as_bytes());
    let accepted = app
        .execute_for_local_owner(ApplicationRequest::SubmitSessionInput {
            mutation_request_id: MutationRequestId::from_bytes([0xe3; 16]),
            session_id,
            text: "Inspect picture.png".into(),
            attachments: Vec::new(),
            service: ModelService::OpenAiChatGpt,
            model_id: model.into(),
        })
        .await
        .unwrap();
    let ApplicationOutcome::Response(ApplicationResponse::SessionInputAccepted { run, .. }) =
        accepted
    else {
        panic!("expected run")
    };
    assert_eq!(run.service, ModelService::OpenAiChatGpt);
    assert_eq!(run.model_id, model);
    assert_eq!(run.protocol_revision, 5);
    assert_eq!(run.credential_generation, 1);
    assert_eq!(
        wait_for_terminal(&app, session_id, run.id).await,
        RunState::Succeeded
    );
    server.await.unwrap();
    app.shutdown().await;
    drop(app);
    let db = rusqlite::Connection::open(root.path().join("data/sessions.sqlite3")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT open_code_service FROM run_accepted_facts",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(
        db.query_row("SELECT model_id FROM run_accepted_facts", [], |row| row
            .get::<_, String>(
            0
        ))
        .unwrap(),
        model
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM session_entries WHERE text LIKE '%synthetic-native-continuation%'",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    drop(db);
    let _store = SessionStore::open_for_test(root.path()).expect("native history should reopen");
}
