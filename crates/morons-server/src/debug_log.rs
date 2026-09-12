use std::{
    io::{self, Write},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::Serialize;

mod runtime;
pub use runtime::{
    DebugLocation, DebugNormalizationStage, DebugResource, DebugToolError, DebugToolKind,
};

use crate::provider::ProviderError;

const PREFIX: &[u8] = b"MORONS_DEBUG ";
const QUEUE_CAPACITY: usize = 32;
const MAX_ENCODED_BYTES: usize = 1024;
const MAX_RECORD_ATTEMPTS: u64 = 512;
const SHUTDOWN_GRACE: Duration = Duration::from_millis(100);
const POLL_INTERVAL: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugService {
    Zen,
    Go,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugStage {
    Preparing,
    Headers,
    HeaderFraming,
    HttpStatus,
    ContentType,
    BodyFraming,
    SseFraming,
    Json,
    Envelope,
    Identity,
    Choices,
    Delta,
    ToolCall,
    Usage,
    Termination,
    FinishReason,
    ArgumentJson,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugFinish {
    Absent,
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Sensitive,
    NetworkError,
    ContextWindowExceeded,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DebugEvent {
    Resource {
        location: DebugLocation,
        resource: DebugResource,
    },
    Normalization {
        location: DebugLocation,
        stage: DebugNormalizationStage,
        resource_limit: bool,
    },
    ChildTool {
        location: DebugLocation,
        child_attempt: u16,
        tool_ordinal: u16,
        tool: DebugToolKind,
        error: Option<DebugToolError>,
        exit_code: Option<i32>,
        signal: Option<u16>,
    },
    Started,
    Provider {
        attempt_id: u64,
        service: DebugService,
        protocol: u16,
        requested_output_tokens: u32,
        stage: DebugStage,
        finish: DebugFinish,
        done: bool,
        usage_seen: bool,
        receipt_accepted: bool,
        error: Option<ProviderError>,
    },
    Child {
        session_id: [u8; 16],
        task_call_id: [u8; 16],
        child_index: u16,
        child_attempt: u16,
        provider_attempt_id: Option<u64>,
        receipt_accepted: bool,
        error: Option<ProviderError>,
    },
}

#[derive(Serialize)]
struct Envelope {
    format_version: u16,
    sequence: u64,
    #[serde(flatten)]
    event: DebugEvent,
}

#[derive(Default)]
struct Registry {
    started: AtomicBool,
    sink: OnceLock<Arc<Sink>>,
}

static GLOBAL: Registry = Registry {
    started: AtomicBool::new(false),
    sink: OnceLock::new(),
};

struct State {
    enabled: AtomicBool,
    record_attempts: AtomicU64,
    attempt_ids: AtomicU64,
}

struct Sink {
    sender: SyncSender<DebugEvent>,
    state: Arc<State>,
}

/// Owns best-effort shutdown; dropping it permanently disables this activation.
pub struct DebugGuard {
    state: Arc<State>,
    worker: Option<JoinHandle<()>>,
}

pub fn start() -> io::Result<DebugGuard> {
    GLOBAL.start(io::stderr())
}

pub fn enabled() -> bool {
    GLOBAL.sink.get().is_some_and(|sink| sink.enabled())
}

pub fn emit(event: DebugEvent) {
    if let Some(sink) = GLOBAL.sink.get() {
        sink.emit(event);
    }
}

pub fn next_attempt_id() -> Option<u64> {
    GLOBAL.sink.get().and_then(|sink| sink.next_attempt_id())
}

impl Registry {
    fn start(&self, writer: impl Write + Send + 'static) -> io::Result<DebugGuard> {
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "debug startup already attempted",
            ));
        }
        let (sender, receiver) = sync_channel(QUEUE_CAPACITY);
        let state = Arc::new(State {
            enabled: AtomicBool::new(true),
            record_attempts: AtomicU64::new(0),
            attempt_ids: AtomicU64::new(0),
        });
        let worker_state = Arc::clone(&state);
        let worker = thread::Builder::new()
            .name("morons-debug".into())
            .spawn(move || write_events(writer, receiver, worker_state))?;
        let sink = Arc::new(Sink {
            sender,
            state: Arc::clone(&state),
        });
        sink.emit(DebugEvent::Started);
        // The one-shot startup reservation makes publication uncontested.
        let _ = self.sink.set(sink);
        Ok(DebugGuard {
            state,
            worker: Some(worker),
        })
    }
}

fn reserve(counter: &AtomicU64, limit: u64) -> Option<u64> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            if value < limit {
                value.checked_add(1)
            } else {
                None
            }
        })
        .ok()
        .and_then(|value| value.checked_add(1))
}

impl Sink {
    fn enabled(&self) -> bool {
        self.state.enabled.load(Ordering::Acquire)
    }

    fn next_attempt_id(&self) -> Option<u64> {
        self.enabled()
            .then(|| reserve(&self.state.attempt_ids, u64::MAX))
            .flatten()
    }

    fn emit(&self, event: DebugEvent) {
        if !self.enabled() || reserve(&self.state.record_attempts, MAX_RECORD_ATTEMPTS).is_none() {
            return;
        }
        match self.sender.try_send(event) {
            Ok(()) | Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => {
                self.state.enabled.store(false, Ordering::Release);
            }
        }
    }
}

fn encode(event: DebugEvent, sequence: u64) -> io::Result<Vec<u8>> {
    let mut line = PREFIX.to_vec();
    serde_json::to_writer(
        &mut line,
        &Envelope {
            format_version: 1,
            sequence,
            event,
        },
    )?;
    line.push(b'\n');
    if line.len() > MAX_ENCODED_BYTES {
        return Err(io::Error::other("debug record exceeds limit"));
    }
    Ok(line)
}

fn write_events(mut writer: impl Write, receiver: Receiver<DebugEvent>, state: Arc<State>) {
    let mut sequence = 0_u64;
    while state.enabled.load(Ordering::Acquire) {
        let event = match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(event) => event,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if !state.enabled.load(Ordering::Acquire) {
            break;
        }
        let Some(next) = sequence.checked_add(1) else {
            break;
        };
        sequence = next;
        let result = encode(event, sequence).and_then(|line| {
            writer.write_all(&line)?;
            writer.flush()
        });
        if result.is_err() {
            break;
        }
    }
    state.enabled.store(false, Ordering::Release);
}

impl Drop for DebugGuard {
    fn drop(&mut self) {
        self.state.enabled.store(false, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return;
        };
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        while !worker.is_finished() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            thread::sleep(POLL_INTERVAL.min(remaining));
        }
        let _ = worker.join();
    }
}

#[cfg(test)]
mod tests;
