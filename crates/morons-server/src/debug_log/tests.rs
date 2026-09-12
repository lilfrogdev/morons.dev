use super::*;
use serde_json::{Value, json};
use std::sync::{Mutex, mpsc};

const COMPLETION_WATCHDOG: Duration = Duration::from_secs(5);

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !predicate() {
        assert!(Instant::now() < deadline, "debug worker did not progress");
        thread::sleep(POLL_INTERVAL);
    }
}

fn decoded(event: DebugEvent) -> Value {
    let line = encode(event, u64::MAX).unwrap();
    assert!(line.len() <= MAX_ENCODED_BYTES);
    assert!(line.starts_with(PREFIX));
    assert_eq!(line.last(), Some(&b'\n'));
    assert_eq!(line.iter().filter(|&&byte| byte == b'\n').count(), 1);
    serde_json::from_slice(&line[PREFIX.len()..]).unwrap()
}

#[test]
fn debug_default_silence_and_one_shot_startup() {
    assert!(!enabled());
    assert_eq!(next_attempt_id(), None);
    emit(DebugEvent::Started);
    assert!(GLOBAL.sink.get().is_none());
    assert!(!GLOBAL.started.load(Ordering::Acquire));

    let registry = Registry::default();
    let capture = Capture::default();
    let guard = registry.start(capture.clone()).unwrap();
    let sink = registry.sink.get().unwrap();
    assert!(sink.enabled());
    assert_eq!(sink.next_attempt_id(), Some(1));
    assert_eq!(
        registry.start(io::sink()).err().unwrap().kind(),
        io::ErrorKind::AlreadyExists
    );
    wait_until(|| capture.0.lock().unwrap().ends_with(b"\n"));
    assert_eq!(
        &*capture.0.lock().unwrap(),
        b"MORONS_DEBUG {\"format_version\":1,\"sequence\":1,\"kind\":\"started\"}\n"
    );
    drop(guard);
    assert!(!sink.enabled());
    assert_eq!(sink.next_attempt_id(), None);
    let before = capture.0.lock().unwrap().clone();
    sink.emit(DebugEvent::Started);
    assert_eq!(*capture.0.lock().unwrap(), before);
    assert_eq!(
        registry.start(io::sink()).err().unwrap().kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn debug_exact_schema_and_maximum_locators() {
    assert_eq!(
        decoded(DebugEvent::Started),
        json!({"format_version":1,"sequence":u64::MAX,"kind":"started"})
    );
    let event = DebugEvent::Provider {
        attempt_id: u64::MAX,
        service: DebugService::Zen,
        protocol: u16::MAX,
        requested_output_tokens: u32::MAX,
        stage: DebugStage::HeaderFraming,
        finish: DebugFinish::ContextWindowExceeded,
        done: false,
        usage_seen: false,
        receipt_accepted: false,
        error: Some(ProviderError::CredentialReauthenticationRequired),
    };
    assert_eq!(
        decoded(event),
        json!({
            "format_version":1,"sequence":u64::MAX,"kind":"provider",
            "attempt_id":u64::MAX,"service":"zen","protocol":u16::MAX,
            "requested_output_tokens":u32::MAX,"stage":"header_framing",
            "finish":"context_window_exceeded","done":false,"usage_seen":false,
            "receipt_accepted":false,"error":"credential_reauthentication_required"
        })
    );
    for error in [
        None,
        Some(ProviderError::CredentialReauthenticationRequired),
    ] {
        assert_eq!(
            decoded(DebugEvent::Child {
                session_id: [255; 16],
                task_call_id: [255; 16],
                child_index: u16::MAX,
                child_attempt: u16::MAX,
                provider_attempt_id: Some(u64::MAX),
                receipt_accepted: false,
                error,
            }),
            json!({
                "format_version":1,"sequence":u64::MAX,"kind":"child",
                "session_id":vec![255;16],"task_call_id":vec![255;16],"child_index":u16::MAX,
                "child_attempt":u16::MAX,"provider_attempt_id":u64::MAX,
                "receipt_accepted":false,"error":error
            })
        );
    }
}

#[test]
fn debug_closed_enum_serialization() {
    macro_rules! check {
        ($ty:ident; $($variant:ident => $name:literal),+ $(,)?) => {
            $(assert_eq!(serde_json::to_value($ty::$variant).unwrap(), json!($name));)+
        };
    }
    check!(DebugService; Zen => "zen", Go => "go");
    check!(DebugStage;
        Preparing => "preparing", Headers => "headers", HeaderFraming => "header_framing",
        HttpStatus => "http_status", ContentType => "content_type", BodyFraming => "body_framing",
        SseFraming => "sse_framing", Json => "json", Envelope => "envelope", Identity => "identity",
        Choices => "choices", Delta => "delta", ToolCall => "tool_call", Usage => "usage",
        Termination => "termination", FinishReason => "finish_reason", ArgumentJson => "argument_json",
        Complete => "complete"
    );
    check!(DebugFinish;
        Absent => "absent", Stop => "stop", ToolCalls => "tool_calls", Length => "length",
        ContentFilter => "content_filter", Sensitive => "sensitive", NetworkError => "network_error",
        ContextWindowExceeded => "context_window_exceeded", Other => "other"
    );
    check!(ProviderError;
        InvalidRequest => "invalid_request", UnsupportedModel => "unsupported_model",
        DataUseRestricted => "data_use_restricted", CredentialGenerationChanged => "credential_generation_changed",
        CredentialNotConfigured => "credential_not_configured",
        CredentialReauthenticationRequired => "credential_reauthentication_required",
        CredentialStoreUnavailable => "credential_store_unavailable", Transport => "transport",
        ResponseHeaderTimeout => "response_header_timeout", StreamInactivityTimeout => "stream_inactivity_timeout",
        TotalTimeout => "total_timeout", Cancelled => "cancelled", RedirectDenied => "redirect_denied",
        UnexpectedContentType => "unexpected_content_type", AuthenticationOrEntitlement => "authentication_or_entitlement",
        RateLimited => "rate_limited", Unavailable => "unavailable", RequestRejected => "request_rejected",
        ProviderExecutionFailed => "provider_execution_failed", MalformedCatalog => "malformed_catalog",
        MalformedResponse => "malformed_response", ResponseLimitExceeded => "response_limit_exceeded",
        IncompleteResponse => "incomplete_response"
    );
}

fn local_sink(capacity: usize) -> (Arc<Sink>, Receiver<DebugEvent>) {
    let (sender, receiver) = sync_channel(capacity);
    (
        Arc::new(Sink {
            sender,
            state: Arc::new(State {
                enabled: AtomicBool::new(true),
                record_attempts: AtomicU64::new(0),
                attempt_ids: AtomicU64::new(0),
            }),
        }),
        receiver,
    )
}

#[test]
fn debug_concurrent_cap_and_checked_ids() {
    let (sink, receiver) = local_sink(MAX_RECORD_ATTEMPTS as usize);
    let ids = thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let sink = &sink;
                scope.spawn(move || {
                    (0..256)
                        .map(|_| {
                            sink.emit(DebugEvent::Started);
                            sink.next_attempt_id().unwrap()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut sorted = ids;
    sorted.sort_unstable();
    assert_eq!(sorted, (1..=2048).collect::<Vec<_>>());
    assert_eq!(
        sink.state.record_attempts.load(Ordering::Acquire),
        MAX_RECORD_ATTEMPTS
    );
    assert_eq!(receiver.try_iter().count(), MAX_RECORD_ATTEMPTS as usize);
    sink.state
        .attempt_ids
        .store(u64::MAX - 1, Ordering::Release);
    assert_eq!(sink.next_attempt_id(), Some(u64::MAX));
    assert_eq!(sink.next_attempt_id(), None);
    assert_eq!(sink.state.attempt_ids.load(Ordering::Acquire), u64::MAX);
}

#[test]
fn debug_queue_backpressure_and_disconnect() {
    let (sink, receiver) = local_sink(QUEUE_CAPACITY);
    let producer_sink = Arc::clone(&sink);
    let (done_tx, done_rx) = mpsc::channel();
    let producer = thread::spawn(move || {
        for _ in 0..MAX_RECORD_ATTEMPTS + 100 {
            producer_sink.emit(DebugEvent::Started);
        }
        let _ = done_tx.send(());
    });
    // Nothing drains the full queue until the producer proves completion.
    let completed = done_rx.recv_timeout(COMPLETION_WATCHDOG);
    if completed.is_err() {
        sink.state.enabled.store(false, Ordering::Release);
    }
    let queued = receiver.try_iter().count();
    sink.emit(DebugEvent::Started);
    let capped = receiver.try_recv().is_err();
    drop(receiver); // Unblock an accidentally blocking producer before joining.
    let joined = producer.join();
    assert!(completed.is_ok(), "producer waited for a full queue");
    assert!(joined.is_ok());
    assert_eq!(queued, QUEUE_CAPACITY);
    assert_eq!(
        sink.state.record_attempts.load(Ordering::Acquire),
        MAX_RECORD_ATTEMPTS
    );
    assert!(capped);
    let (sink, receiver) = local_sink(QUEUE_CAPACITY);
    drop(receiver);
    sink.emit(DebugEvent::Started);
    assert!(!sink.enabled());
    assert_eq!(sink.next_attempt_id(), None);
}

struct FailWriter {
    flush_error: bool,
}

impl Write for FailWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.flush_error {
            Ok(bytes.len())
        } else {
            Err(io::Error::other("private writer detail"))
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("private flush detail"))
    }
}

#[test]
fn debug_writer_errors_disable_diagnostics() {
    for flush_error in [false, true] {
        let registry = Registry::default();
        let guard = registry.start(FailWriter { flush_error }).unwrap();
        let sink = registry.sink.get().unwrap();
        wait_until(|| !sink.enabled());
        assert_eq!(sink.next_attempt_id(), None);
        sink.emit(DebugEvent::Started);
        assert_eq!(sink.state.record_attempts.load(Ordering::Acquire), 1);
        drop(guard);
    }
}

struct BlockedWriter {
    entered: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
}

impl Write for BlockedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(release) = self.release.take() {
            let _ = self.entered.send(());
            let _ = release.recv(); // Sender drop also releases failure cleanup.
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn debug_blocked_writer_does_not_hold_producers_or_shutdown() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let registry = Registry::default();
    let guard = registry
        .start(BlockedWriter {
            entered: entered_tx,
            release: Some(release_rx),
        })
        .unwrap();
    let entered = entered_rx.recv_timeout(COMPLETION_WATCHDOG);
    let sink = registry.sink.get().unwrap();
    let producer_sink = Arc::clone(sink);
    let (done_tx, done_rx) = mpsc::channel();
    let producer = thread::spawn(move || {
        for _ in 0..1024 {
            producer_sink.emit(DebugEvent::Started);
        }
        let _ = done_tx.send("produced");
        drop(guard);
        let _ = done_tx.send("stopped");
    });
    let produced = done_rx.recv_timeout(COMPLETION_WATCHDOG);
    let stopped = done_rx.recv_timeout(COMPLETION_WATCHDOG);
    let disabled = !sink.enabled();
    // Check completion while blocked, then release/join even on a failed check.
    sink.state.enabled.store(false, Ordering::Release);
    drop(release_tx);
    let joined = producer.join();
    wait_until(|| Arc::strong_count(&sink.state) == 1);
    assert!(entered.is_ok());
    assert_eq!(produced, Ok("produced"));
    assert_eq!(stopped, Ok("stopped"));
    assert!(joined.is_ok());
    assert!(disabled);
}

#[test]
fn debug_writer_sequences_are_positive_and_ordered() {
    let (sink, receiver) = local_sink(MAX_RECORD_ATTEMPTS as usize);
    for _ in 0..MAX_RECORD_ATTEMPTS {
        sink.emit(DebugEvent::Started);
    }
    let state = Arc::clone(&sink.state);
    drop(sink);
    let capture = Capture::default();
    write_events(capture.clone(), receiver, state);
    let bytes = capture.0.lock().unwrap();
    let lines: Vec<_> = bytes.split_inclusive(|&byte| byte == b'\n').collect();
    assert_eq!(lines.len(), MAX_RECORD_ATTEMPTS as usize);
    for (index, line) in lines.into_iter().enumerate() {
        assert!(line.len() <= MAX_ENCODED_BYTES);
        let value: Value = serde_json::from_slice(&line[PREFIX.len()..]).unwrap();
        assert_eq!(value["sequence"], json!(index + 1));
    }
}

#[test]
fn debug_runtime_closed_conversions_and_default_silence() {
    use crate::{persistence::PersistenceResourceLimit as P, tools::ToolKind as T};
    for (source, expected) in [
        (P::Sessions, "sessions"),
        (P::Runs, "runs"),
        (P::Context, "context"),
        (P::Transcript, "transcript"),
        (P::LogicalSequence, "logical_sequence"),
        (P::CredentialGeneration, "credential_generation"),
        (P::CredentialMutations, "credential_mutations"),
        (P::ModelSelections, "model_selections"),
        (P::DataUsePolicies, "data_use_policies"),
    ] {
        assert_eq!(
            serde_json::to_value(DebugResource::from(source)).unwrap(),
            json!(expected)
        );
    }
    for (source, expected) in [
        (T::Read, "read"),
        (T::Write, "write"),
        (T::Edit, "edit"),
        (T::Bash, "bash"),
        (T::WebSearch, "web_search"),
        (T::Ipython, "ipython"),
        (T::ListDirectory, "legacy"),
    ] {
        assert_eq!(
            serde_json::to_value(DebugToolKind::from(source)).unwrap(),
            json!(expected)
        );
    }
    assert!(!enabled());
    emit(DebugEvent::Resource {
        location: DebugLocation::Root { run_id: [255; 16] },
        resource: DebugResource::Context,
    });
    assert_eq!(next_attempt_id(), None);
    assert!(GLOBAL.sink.get().is_none());
}

#[test]
fn debug_runtime_maximum_records_are_bounded_and_located() {
    let location = DebugLocation::Child {
        session_id: [255; 16],
        task_call_id: [255; 16],
        child_index: u16::MAX,
    };
    let events = [
        DebugEvent::Resource {
            location: DebugLocation::Root { run_id: [255; 16] },
            resource: DebugResource::CredentialGeneration,
        },
        DebugEvent::Resource {
            location,
            resource: DebugResource::ChildContextBudget,
        },
        DebugEvent::Normalization {
            location,
            stage: DebugNormalizationStage::ForbiddenChildTool,
            resource_limit: false,
        },
        DebugEvent::ChildTool {
            location,
            child_attempt: u16::MAX,
            tool_ordinal: u16::MAX,
            tool: DebugToolKind::Bash,
            error: None,
            exit_code: Some(i32::MIN),
            signal: Some(u16::MAX),
        },
        DebugEvent::ChildTool {
            location,
            child_attempt: 1,
            tool_ordinal: 2,
            tool: DebugToolKind::Edit,
            error: Some(DebugToolError::ReplacementAmbiguous),
            exit_code: None,
            signal: None,
        },
        DebugEvent::ChildTool {
            location,
            child_attempt: 2,
            tool_ordinal: 3,
            tool: DebugToolKind::WebSearch,
            error: Some(DebugToolError::WebSearchUncertain),
            exit_code: None,
            signal: None,
        },
    ];
    let expected = events
        .iter()
        .map(|event| serde_json::to_value(event).unwrap())
        .collect::<Vec<_>>();
    let (sink, receiver) = local_sink(events.len());
    for event in events {
        sink.emit(event);
    }
    let state = Arc::clone(&sink.state);
    drop(sink);
    let capture = Capture::default();
    write_events(capture.clone(), receiver, state);
    let bytes = capture.0.lock().unwrap();
    let lines = bytes
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), expected.len());
    for (index, (line, expected)) in lines.into_iter().zip(expected).enumerate() {
        assert!(line.len() <= MAX_ENCODED_BYTES && line.starts_with(PREFIX));
        let mut record: Value = serde_json::from_slice(&line[PREFIX.len()..]).unwrap();
        assert_eq!(
            record.as_object_mut().unwrap().remove("format_version"),
            Some(json!(1))
        );
        assert_eq!(
            record.as_object_mut().unwrap().remove("sequence"),
            Some(json!(index + 1))
        );
        assert_eq!(record, expected);
    }
}
