use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

mod daily;
mod runtime;
pub(crate) use runtime::tool_limit_event;
pub use runtime::{
    DebugLocation, DebugNormalizationStage, DebugResource, DebugToolError, DebugToolKind,
};

use crate::provider::ProviderError;

const PREFIX: &[u8] = b"MORONS_DEBUG ";
const QUEUE_CAPACITY: usize = 80;
const MAX_ENCODED_BYTES: usize = 1024;
const MAX_RECORD_ATTEMPTS: u64 = 512;
const RATE_WINDOW: Duration = Duration::from_secs(60);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugUsageRejection {
    Missing,
    Schema,
    InputLimit,
    OutputLimit,
    TotalLimit,
    CachedInput,
    CacheWriteInput,
    ReasoningOutput,
    TotalMismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugStartupStage {
    Total,
    EndpointPrepare,
    ApplicationOpen,
    EndpointPublish,
    DatabaseInitialize,
    DatabaseFileValidation,
    DatabaseConnectionOpen,
    DatabaseConfiguration,
    DatabaseMigration,
    DatabaseSchemaValidation,
    DatabaseQuickValidation,
    FactValidation,
    ProjectionRebuild,
    IntegrityValidation,
    BackendRecovery,
    AttachmentReconciliation,
    ContextIntegrity,
    DataUsePolicyValidation,
    TaskBindingValidation,
    WebBindingValidation,
    CheckpointDigestValidation,
    MaintenanceValidation,
    CompactionRecovery,
    CredentialMutationRecovery,
    CredentialRefreshRecovery,
    MaintenanceRecovery,
    SessionCreationRecovery,
    ChildJournalRecovery,
    ToolRecovery,
    LocalCommandRecovery,
    RunRecovery,
    SessionArchiveRecovery,
    SessionDeleteRecovery,
}

pub fn startup_stage<T, E>(
    stage: DebugStartupStage,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    startup_stage_with_sink(GLOBAL.sink.get().map(Arc::as_ref), stage, operation)
}

fn startup_stage_with_sink<T, E>(
    sink: Option<&Sink>,
    stage: DebugStartupStage,
    operation: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    let Some(sink) = sink.filter(|sink| sink.enabled()) else {
        return operation();
    };
    sink.emit(DebugEvent::Startup {
        stage,
        began: true,
        success: None,
        elapsed_us: None,
    });
    let started = Instant::now();
    let result = operation();
    sink.emit(DebugEvent::Startup {
        stage,
        began: false,
        success: Some(result.is_ok()),
        elapsed_us: Some(started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64),
    });
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugCitationRejection {
    StreamMetadata,
    StreamAnnotation,
    StreamConsistency,
    Annotations,
    AnnotationType,
    Url,
    Title,
    Offsets,
    TitleLimit,
    OffsetOrder,
    OffsetBounds,
    MissingCitations,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugContextCheck {
    ToolInputBytes,
    ToolResultBytes,
    InputTokens,
    SourceBytes,
    ReservedEntries,
    ImageCount,
    ImageBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugProviderStage {
    Compaction,
    Request,
    PrepareDispatch,
    Execute,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DebugEvent {
    ProviderFailure {
        session_id: [u8; 16],
        run_id: [u8; 16],
        stage: DebugProviderStage,
        error: ProviderError,
        uncertain: bool,
    },
    ContextLimit {
        run_id: [u8; 16],
        call_id: Option<[u8; 16]>,
        check: DebugContextCheck,
        measured: u64,
        limit: u64,
    },
    ToolLimit {
        run_id: [u8; 16],
        call_id: [u8; 16],
        tool: DebugToolKind,
        error: DebugToolError,
        stdout_bytes: Option<usize>,
        stderr_bytes: Option<usize>,
        per_stream_limit_bytes: Option<usize>,
    },
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
        child_attempt: u64,
        tool_ordinal: u64,
        tool: DebugToolKind,
        error: Option<DebugToolError>,
        exit_code: Option<i32>,
        signal: Option<u16>,
    },
    WebCitation {
        reason: DebugCitationRejection,
    },
    WebUsage {
        reason: DebugUsageRejection,
    },
    WebSearch {
        stage: crate::web_diagnostic::WebStage,
        category: crate::web_diagnostic::WebCategory,
    },
    Dropped {
        records: u64,
    },
    Started,
    Startup {
        stage: DebugStartupStage,
        began: bool,
        success: Option<bool>,
        elapsed_us: Option<u64>,
    },
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
        child_attempt: u64,
        provider_attempt_id: Option<u64>,
        receipt_accepted: bool,
        error: Option<ProviderError>,
    },
}

#[derive(Serialize)]
struct Envelope {
    format_version: u16,
    sequence: u64,
    timestamp_unix_ms: u64,
    process_id: u32,
    level: &'static str,
    component: &'static str,
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
    failed: AtomicBool,
    daily_capped: AtomicBool,
    record_attempts: AtomicU64,
    window: Mutex<Instant>,
    dropped: AtomicU64,
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

static LOG_DIRECTORY: OnceLock<String> = OnceLock::new();

pub fn status() -> morons_protocol::ApplicationResponse {
    let state = GLOBAL.sink.get().map(|sink| &sink.state);
    morons_protocol::ApplicationResponse::DebugStatus {
        active: state.is_some_and(|s| s.enabled.load(Ordering::Acquire)),
        failed: state.is_some_and(|s| s.failed.load(Ordering::Acquire)),
        daily_capped: state.is_some_and(|s| s.daily_capped.load(Ordering::Acquire)),
        dropped_records: state.map_or(0, |s| s.dropped.load(Ordering::Acquire)),
        log_directory: LOG_DIRECTORY.get().cloned(),
    }
}

pub fn start(root: &std::path::Path) -> io::Result<DebugGuard> {
    let guard = GLOBAL.start(daily::DailyLog::open(root)?)?;
    let _ = LOG_DIRECTORY.set(root.join("logs").to_string_lossy().into_owned());
    Ok(guard)
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
            failed: AtomicBool::new(false),
            daily_capped: AtomicBool::new(false),
            record_attempts: AtomicU64::new(0),
            window: Mutex::new(Instant::now()),
            dropped: AtomicU64::new(0),
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
        if !self.enabled() {
            return;
        }
        let Ok(mut window) = self.state.window.try_lock() else {
            self.state.drop_record();
            return;
        };
        if window.elapsed() >= RATE_WINDOW {
            *window = Instant::now();
            self.state.record_attempts.store(0, Ordering::Release);
        }
        if reserve(&self.state.record_attempts, MAX_RECORD_ATTEMPTS).is_none() {
            self.state.drop_record();
            return;
        }
        drop(window);
        match self.sender.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.state.drop_record(),
            Err(TrySendError::Disconnected(_)) => {
                self.state.enabled.store(false, Ordering::Release);
            }
        }
    }
}

fn timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl DebugEvent {
    fn component(&self) -> &'static str {
        match self {
            Self::Started | Self::Dropped { .. } => "logger",
            Self::Startup { .. } => "startup",
            Self::Provider { .. } | Self::ProviderFailure { .. } => "provider",
            Self::WebSearch { .. } | Self::WebCitation { .. } | Self::WebUsage { .. } => {
                "web_search"
            }
            _ => "tools",
        }
    }

    fn level(&self) -> &'static str {
        match self {
            Self::Dropped { .. }
            | Self::WebCitation { .. }
            | Self::WebUsage { .. }
            | Self::Startup {
                success: Some(false),
                ..
            }
            | Self::Provider { error: Some(_), .. }
            | Self::Child { error: Some(_), .. }
            | Self::ProviderFailure { .. }
            | Self::ContextLimit { .. }
            | Self::ToolLimit { .. }
            | Self::Resource { .. }
            | Self::Normalization {
                resource_limit: true,
                ..
            }
            | Self::ChildTool { error: Some(_), .. } => "warn",
            Self::Started => "info",
            _ => "debug",
        }
    }
}

impl State {
    fn drop_record(&self) {
        let _ = reserve(&self.dropped, u64::MAX);
    }
}

fn encode(event: DebugEvent, sequence: u64) -> io::Result<Vec<u8>> {
    let mut line = PREFIX.to_vec();
    serde_json::to_writer(
        &mut line,
        &Envelope {
            format_version: 2,
            sequence,
            timestamp_unix_ms: timestamp_ms(),
            process_id: std::process::id(),
            level: event.level(),
            component: event.component(),
            event,
        },
    )?;
    line.push(b'\n');
    if line.len() > MAX_ENCODED_BYTES {
        return Err(io::Error::other("debug record exceeds limit"));
    }
    Ok(line)
}

fn write_events(writer: impl Write, receiver: Receiver<DebugEvent>, state: Arc<State>) {
    write_events_with_interval(writer, receiver, state, RATE_WINDOW);
}

fn write_events_with_interval(
    mut writer: impl Write,
    receiver: Receiver<DebugEvent>,
    state: Arc<State>,
    summary_interval: Duration,
) {
    let mut sequence = 0_u64;
    let mut summary_at = Instant::now();
    let mut reported_drops = 0;
    while state.enabled.load(Ordering::Acquire) {
        if summary_at.elapsed() >= summary_interval {
            let drops = state.dropped.load(Ordering::Acquire);
            if drops > reported_drops {
                let Some(next) = sequence.checked_add(1) else {
                    break;
                };
                sequence = next;
                let result = encode(
                    DebugEvent::Dropped {
                        records: drops - reported_drops,
                    },
                    sequence,
                )
                .and_then(|line| writer.write_all(&line).and_then(|()| writer.flush()));
                if let Err(error) = result {
                    if error.kind() == io::ErrorKind::WouldBlock {
                        state.daily_capped.store(true, Ordering::Release);
                    } else {
                        state.failed.store(true, Ordering::Release);
                        break;
                    }
                } else {
                    state.daily_capped.store(false, Ordering::Release);
                    reported_drops = drops;
                }
            }
            summary_at = Instant::now();
        }
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
        if let Err(error) = result {
            if error.kind() == io::ErrorKind::WouldBlock {
                state.drop_record();
                state.daily_capped.store(true, Ordering::Release);
                continue;
            }
            state.failed.store(true, Ordering::Release);
            break;
        }
        state.daily_capped.store(false, Ordering::Release);
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
