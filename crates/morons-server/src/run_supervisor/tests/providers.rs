use super::*;

pub(super) async fn spawn_catalog_provider() -> (
    String,
    oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("catalog fixture should bind");
    let address = listener
        .local_addr()
        .expect("catalog fixture should have an address");
    let (captured_sender, captured_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("catalog should connect");
        let request = read_http_request(&mut stream).await;
        captured_sender
            .send(String::from_utf8(request).expect("catalog request should be UTF-8"))
            .unwrap_or_else(|_| panic!("catalog request should be observed"));
        let body = concat!(
            "{\"object\":\"list\",\"data\":[",
            "{\"id\":\"gpt-5.6-luna\",\"object\":\"model\",\"created\":1,\"owned_by\":\"opencode\"},",
            "{\"id\":\"glm-5.3-flash\",\"object\":\"model\",\"created\":1,\"owned_by\":\"opencode\"},",
            "{\"id\":\"muse-spark-1.2-contributor\",\"object\":\"model\",\"created\":1,\"owned_by\":\"opencode\"},",
            "{\"id\":\"qwen3.8-max\",\"object\":\"model\",\"created\":1,\"owned_by\":\"opencode\"}",
            "]}"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("catalog response should be written");
        stream
            .shutdown()
            .await
            .expect("catalog response should close");
    });
    (format!("http://{address}"), captured_receiver, server)
}

pub(super) async fn spawn_compaction_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("compaction provider should bind");
    let address = listener.local_addr().expect("provider address should load");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let answers = ["SUMMARY_OF_OLDEST_PREFIX", "compacted answer"];
        let mut captured = Vec::new();
        for (index, answer) in answers.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("request should be UTF-8"),
            );
            let request = request_body(captured.last().unwrap());
            assert_eq!(request["model"], "muse-spark-1.2");
            assert_eq!(request["stream"], true);
            assert_eq!(request["store"], false);
            assert_eq!(request["input"][0]["role"], "developer");
            if index == 0 {
                assert!(request["tools"].as_array().unwrap().is_empty());
                assert_eq!(request["max_output_tokens"], 4_096);
                assert_eq!(request["input"].as_array().unwrap().len(), 2);
                assert_eq!(request["input"][1]["role"], "user");
            } else {
                let names: Vec<_> = request["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|tool| tool["name"].as_str().unwrap())
                    .collect();
                assert_eq!(
                    names,
                    ["read", "write", "edit", "bash", "web_search", "ipython"]
                );
            }
            let response_id = format!("resp_compact_{}", index + 1);
            let message_id = format!("msg_compact_{}", index + 1);
            let output = format!(
                "{{\"id\":\"{message_id}\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{{\"type\":\"output_text\",\"text\":\"{answer}\",\"annotations\":[]}}]}}"
            );
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":20,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":5,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":25}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("response should write");
            stream.shutdown().await.expect("response should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

pub(super) async fn spawn_image_provider() -> (
    String,
    oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("image provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("image provider fixture should have an address");
    let (captured_sender, captured_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let request = read_http_request(&mut stream).await;
        captured_sender
            .send(String::from_utf8(request).expect("request should be UTF-8"))
            .unwrap_or_else(|_| panic!("request should be observed"));
        let output = "{\"id\":\"msg_image\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"I see the image.\",\"annotations\":[]}]}";
        let body = format!(
            "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"resp_image\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-5.4\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"resp_image\",\"object\":\"response\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":20,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":5,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":25}}}}}}\n\ndata: [DONE]\n\n"
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("image headers should write");
        stream
            .write_all(body.as_bytes())
            .await
            .expect("image response should write");
        stream
            .shutdown()
            .await
            .expect("image response should close");
    });
    (format!("http://{address}"), captured_receiver, server)
}

pub(super) async fn spawn_successful_provider() -> (
    String,
    oneshot::Receiver<String>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("provider fixture should have an address");
    let (captured_sender, captured_receiver) = oneshot::channel();
    let (complete_sender, complete_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        let request = read_http_request(&mut stream).await;
        let captured = String::from_utf8(request).expect("request should be UTF-8");
        captured_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("request should be observed"));
        let first = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_run_1\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"output_index\":0,\"content_index\":0,\"item_id\":\"msg_run_1\",\"delta\":\"durable answer\",\"logprobs\":[]}\n\n"
        );
        let terminal = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"resp_run_1\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{\"id\":\"msg_run_1\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"durable answer\",\"annotations\":[]}]}],\"usage\":{\"input_tokens\":8,\"input_tokens_details\":{\"cached_tokens\":0},\"output_tokens\":3,\"output_tokens_details\":{\"reasoning_tokens\":0},\"total_tokens\":11}}}\n\n",
            "data: [DONE]\n\n"
        );
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            first.len() + terminal.len()
        );
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("response headers should be written");
        stream
            .write_all(first.as_bytes())
            .await
            .expect("streaming response should begin");
        complete_receiver
            .await
            .unwrap_or_else(|_| panic!("provider completion should be released"));
        stream
            .write_all(terminal.as_bytes())
            .await
            .expect("terminal response should be written");
        stream.shutdown().await.expect("response should close");
    });
    (
        format!("http://{address}"),
        captured_receiver,
        complete_sender,
        server,
    )
}

pub(super) async fn write_provider_output(
    stream: &mut tokio::net::TcpStream,
    response_id: &str,
    output: &str,
) {
    let body = provider_output_body(response_id, output);
    write_provider_headers(stream, body.len()).await;
    stream
        .write_all(body.as_bytes())
        .await
        .expect("provider response should write");
    stream
        .shutdown()
        .await
        .expect("provider response should close");
}

pub(super) fn provider_output_body(response_id: &str, output: &str) -> String {
    format!(
        "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
    )
}

pub(super) async fn write_provider_headers(
    stream: &mut tokio::net::TcpStream,
    content_length: usize,
) {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(headers.as_bytes())
        .await
        .expect("provider response headers should write");
}

pub(super) async fn spawn_direct_tool_loop_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("tool provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("tool provider should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let read_arguments = r#"{"path":"note.txt","offset":1,"limit":20}"#;
        let edit_arguments =
            r#"{"path":"note.txt","replacements":[{"old_text":"before","new_text":"after"}]}"#;
        let bash_arguments = r#"{"command":"printf shell > shell.txt; printf 'shell stdout'"}"#;
        let outputs = [
            format!(
                "{{\"id\":\"fc_read\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_read\",\"name\":\"read\",\"arguments\":{}}}",
                serde_json::to_string(read_arguments).expect("arguments should encode")
            ),
            format!(
                "{{\"id\":\"fc_edit\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_edit\",\"name\":\"edit\",\"arguments\":{}}}",
                serde_json::to_string(edit_arguments).expect("arguments should encode")
            ),
            format!(
                "{{\"id\":\"fc_bash\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_bash\",\"name\":\"bash\",\"arguments\":{}}}",
                serde_json::to_string(bash_arguments).expect("arguments should encode")
            ),
            "{\"id\":\"msg_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Updated note.txt.\",\"annotations\":[]}] }".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener
                .accept()
                .await
                .expect("tool request should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("tool request should be UTF-8"),
            );
            let response_id = format!("resp_tool_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("tool response headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("tool response should write");
            stream.shutdown().await.expect("tool response should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("tool requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

pub(super) async fn spawn_read_image_tool_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("read image provider should bind");
    let address = listener
        .local_addr()
        .expect("provider should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let arguments = r#"{"path":"picture.png","offset":1,"limit":1}"#;
        let outputs = [
            format!(
                "{{\"id\":\"fc_read_image\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_read_image\",\"name\":\"read\",\"arguments\":{}}}",
                serde_json::to_string(arguments).expect("arguments should encode")
            ),
            "{\"id\":\"msg_image_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"I inspected picture.png.\",\"annotations\":[]}]}".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("provider request should be UTF-8"),
            );
            let response_id = format!("resp_read_image_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"gpt-5.4\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":16,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":4,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":20}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("provider headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("provider response should write");
            stream.shutdown().await.expect("provider should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

pub(super) async fn spawn_ipython_tool_loop_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("IPython provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("IPython provider fixture should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let cells = [r#"{"cell":"value = 41"}"#, r#"{"cell":"value + 1"}"#];
        let outputs = [
            format!(
                "{{\"id\":\"fc_python_1\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_python_1\",\"name\":\"ipython\",\"arguments\":{}}}",
                serde_json::to_string(cells[0]).expect("arguments should encode")
            ),
            format!(
                "{{\"id\":\"fc_python_2\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_python_2\",\"name\":\"ipython\",\"arguments\":{}}}",
                serde_json::to_string(cells[1]).expect("arguments should encode")
            ),
            "{\"id\":\"msg_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Persistent Python returned 42.\",\"annotations\":[]}]}".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("provider request should be UTF-8"),
            );
            let response_id = format!("resp_ipython_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("provider headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("provider response should write");
            stream.shutdown().await.expect("provider should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("provider requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

pub(super) async fn spawn_web_search_tool_loop_provider() -> (
    String,
    oneshot::Receiver<Vec<String>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("web tool provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("web tool provider fixture should have an address");
    let (requests_sender, requests_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let search_arguments = r#"{"query":"current Rust release"}"#;
        let outputs = [
            format!(
                "{{\"id\":\"fc_web\",\"type\":\"function_call\",\"status\":\"completed\",\"call_id\":\"provider_web\",\"name\":\"web_search\",\"arguments\":{}}}",
                serde_json::to_string(search_arguments).expect("arguments should encode")
            ),
            "{\"id\":\"msg_final\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"output_text\",\"text\":\"Found the Rust site.\",\"annotations\":[]}]}".to_owned(),
        ];
        let mut captured = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("provider should connect");
            captured.push(
                String::from_utf8(read_http_request(&mut stream).await)
                    .expect("provider request should be UTF-8"),
            );
            let response_id = format!("resp_web_{}", index + 1);
            let body = format!(
                "event: response.created\ndata: {{\"type\":\"response.created\",\"sequence_number\":0,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}}}\n\nevent: response.completed\ndata: {{\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{{\"id\":\"{response_id}\",\"object\":\"response\",\"model\":\"muse-spark-1.2\",\"status\":\"completed\",\"output\":[{output}],\"usage\":{{\"input_tokens\":8,\"input_tokens_details\":{{\"cached_tokens\":0}},\"output_tokens\":3,\"output_tokens_details\":{{\"reasoning_tokens\":0}},\"total_tokens\":11}}}}}}\n\ndata: [DONE]\n\n"
            );
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("provider headers should write");
            stream
                .write_all(body.as_bytes())
                .await
                .expect("provider response should write");
            stream.shutdown().await.expect("provider should close");
        }
        requests_sender
            .send(captured)
            .unwrap_or_else(|_| panic!("provider requests should be observed"));
    });
    (format!("http://{address}"), requests_receiver, server)
}

pub(super) async fn spawn_search_adapter(
    missing_citations: bool,
) -> (
    String,
    oneshot::Receiver<String>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("search fixture should bind");
    let address = listener
        .local_addr()
        .expect("search fixture should have an address");
    let (request_sender, request_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .expect("search deadline")
            .expect("search should connect");
        let request = read_http_request(&mut stream).await;
        request_sender
            .send(String::from_utf8(request).expect("search request should be UTF-8"))
            .unwrap_or_else(|_| panic!("search request should be observed"));
        let mut body =
            String::from_utf8(crate::provider::openai_web::response_sources_fixture()).unwrap();
        if missing_citations {
            body = body
                .lines()
                .map(|line| {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let mut value: serde_json::Value = serde_json::from_str(data).unwrap();
                        if let Some(content) = value
                            .pointer_mut("/item/content")
                            .and_then(|v| v.as_array_mut())
                        {
                            for part in content {
                                part["annotations"] = serde_json::json!([]);
                            }
                        }
                        if let Some(output) = value
                            .pointer_mut("/response/output")
                            .and_then(|v| v.as_array_mut())
                        {
                            for item in output {
                                if let Some(content) =
                                    item.get_mut("content").and_then(|v| v.as_array_mut())
                                {
                                    for part in content {
                                        part["annotations"] = serde_json::json!([]);
                                    }
                                }
                            }
                        }
                        format!("data: {value}\n")
                    } else {
                        format!("{line}\n")
                    }
                })
                .collect();
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("search response should write");
        stream.shutdown().await.expect("search should close");
    });
    (format!("http://{address}/search"), request_receiver, server)
}

pub(super) async fn spawn_stalled_provider()
-> (String, oneshot::Receiver<()>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("provider fixture should bind");
    let address = listener
        .local_addr()
        .expect("provider fixture should have an address");
    let (dispatched_sender, dispatched_receiver) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("request should connect");
        read_http_request(&mut stream).await;
        let body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_stalled\",\"object\":\"response\",\"status\":\"in_progress\",\"model\":\"muse-spark-1.2\"}}\n\n"
        );
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
        stream
            .write_all(headers.as_bytes())
            .await
            .expect("headers should write");
        stream
            .write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes())
            .await
            .expect("stream chunk should write");
        dispatched_sender
            .send(())
            .unwrap_or_else(|_| panic!("dispatch should be observed"));
        let mut byte = [0_u8; 1];
        match time::timeout(Duration::from_secs(5), stream.read(&mut byte)).await {
            Ok(Ok(0)) => {}
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::BrokenPipe
                ) => {}
            Ok(Ok(_)) => panic!("cancelled request should not send more bytes"),
            Ok(Err(error)) => panic!("cancelled request closed unexpectedly: {error}"),
            Err(_) => panic!("cancelled request should close promptly"),
        }
    });
    (format!("http://{address}"), dispatched_receiver, server)
}
