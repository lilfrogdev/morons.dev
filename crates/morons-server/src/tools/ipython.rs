use std::{
    collections::HashMap,
    ffi::OsString,
    io::{Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OnceCell, OwnedSemaphorePermit, Semaphore};

#[cfg(windows)]
use super::bash::PlatformJob;
#[cfg(unix)]
use super::bash::terminate_process_group;
use super::{
    MAX_IPYTHON_CELL_BYTES, MAX_IPYTHON_OUTPUT_BYTES, ToolErrorKind, ToolInput, ToolOutput,
    ToolResult, bash::terminate_tree,
};
use crate::{persistence::SessionId, provider::ProviderCancellation};

const MAX_CONCURRENT_KERNELS: usize = 4;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const CELL_WALL_TIME_LIMIT: Duration = Duration::from_secs(5 * 60);
const CELL_INACTIVITY_LIMIT: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_BRIDGE_LINE_BYTES: usize = 128 * 1024;
const BRIDGE_CHANNEL_CAPACITY: usize = 32;

const BRIDGE_SOURCE: &str = r#"
import json
import os
import queue
import subprocess
import sys
import threading
from queue import Empty

from jupyter_client import KernelManager

commands = queue.Queue()


def emit(message):
    sys.stdout.write(json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def emit_text(request_id, channel, value):
    if not isinstance(value, str):
        value = str(value)
    for start in range(0, len(value), 4096):
        emit({"type": "output", "id": request_id, "channel": channel, "text": value[start:start + 4096]})


def read_commands():
    for line in sys.stdin:
        try:
            command = json.loads(line)
        except Exception:
            continue
        commands.put(command)
    commands.put({"type": "shutdown"})


reader = threading.Thread(target=read_commands, daemon=True)
reader.start()
manager = KernelManager(kernel_name="python3")
client = None
kernel_stream = open(os.devnull, "r+b", buffering=0)
try:
    manager.start_kernel(cwd=os.getcwd(), stdin=kernel_stream, stdout=kernel_stream, stderr=kernel_stream)
    client = manager.client()
    client.start_channels()
    client.wait_for_ready(timeout=20)
    kernel_process_group = getattr(manager.provisioner, "pgid", None)
    emit({"type": "ready", "kernel_process_group": kernel_process_group})
    stopping = False
    while not stopping:
        command = commands.get()
        if command.get("type") == "shutdown":
            break
        if command.get("type") != "execute":
            continue
        request_id = command.get("id")
        code = command.get("code")
        if not isinstance(request_id, int) or not isinstance(code, str):
            emit({"type": "protocol_error"})
            continue
        message_id = client.execute(code, allow_stdin=False, stop_on_error=True, store_history=True)
        status = "ok"
        execution_count = None
        while True:
            try:
                message = client.get_iopub_msg(timeout=0.1)
            except Empty:
                continue
            if message.get("parent_header", {}).get("msg_id") != message_id:
                continue
            message_type = message.get("header", {}).get("msg_type")
            content = message.get("content", {})
            if message_type == "execute_input":
                execution_count = content.get("execution_count")
            elif message_type == "stream":
                channel = "stderr" if content.get("name") == "stderr" else "stdout"
                emit_text(request_id, channel, content.get("text", ""))
            elif message_type in ("execute_result", "display_data", "update_display_data"):
                data = content.get("data", {})
                if "text/plain" in data:
                    emit_text(request_id, "display", data.get("text/plain", ""))
                else:
                    emit_text(request_id, "display", "[non-text IPython output omitted]")
                if message_type == "execute_result":
                    execution_count = content.get("execution_count")
            elif message_type == "error":
                status = "error"
                traceback = content.get("traceback", [])
                if isinstance(traceback, list):
                    emit_text(request_id, "stderr", "\n".join(str(item) for item in traceback))
                else:
                    emit_text(request_id, "stderr", str(traceback))
            elif message_type == "status" and content.get("execution_state") == "idle":
                break
        emit({"type": "complete", "id": request_id, "status": status, "execution_count": execution_count})
finally:
    if client is not None:
        try:
            client.stop_channels()
        except Exception:
            pass
    try:
        manager.shutdown_kernel(now=True)
    except Exception:
        pass
    kernel_stream.close()
"#;

#[cfg(test)]
const TEST_BRIDGE_SOURCE: &str = r#"
import json
import os
import subprocess
import sys
import time

values = {}
count = 0
print(json.dumps({"type": "ready", "kernel_process_group": getattr(os, "getpgrp", lambda: None)()}, separators=(",", ":")), flush=True)
for line in sys.stdin:
    command = json.loads(line)
    if command.get("type") != "execute":
        continue
    request_id = command["id"]
    code = command["code"]
    count += 1
    status = "ok"
    if code == "value = 41":
        values["value"] = 41
    elif code == "value + 1":
        if "value" in values:
            print(json.dumps({"type": "output", "id": request_id, "channel": "display", "text": str(values["value"] + 1)}, separators=(",", ":")), flush=True)
        else:
            status = "error"
            print(json.dumps({"type": "output", "id": request_id, "channel": "stderr", "text": "NameError: value is not defined"}, separators=(",", ":")), flush=True)
    elif code == "cwd":
        print(json.dumps({"type": "output", "id": request_id, "channel": "stdout", "text": os.getcwd()}, separators=(",", ":")), flush=True)
    elif code.startswith("spawn:"):
        started, leaked = code[6:].split("|", 1)
        with open(started, "w", encoding="utf-8") as marker:
            marker.write("started")
        subprocess.Popen([sys.executable, "-c", "import sys,time;time.sleep(2);open(sys.argv[1],'w').write('leaked')", leaked])
        while True:
            time.sleep(1)
    print(json.dumps({"type": "complete", "id": request_id, "status": status, "execution_count": count}, separators=(",", ":")), flush=True)
"#;

type KernelCell = Arc<OnceCell<Arc<StdMutex<KernelProcess>>>>;

pub(crate) struct IpythonSupervisor {
    kernels: Mutex<KernelRegistry>,
    permits: Arc<Semaphore>,
    stopping: AtomicBool,
    bridge: Arc<BridgeConfiguration>,
}

struct BridgeConfiguration {
    executable: OsString,
    source: &'static str,
}

struct KernelRegistry {
    entries: HashMap<SessionId, KernelRegistryEntry>,
    usage_sequence: u64,
}

struct KernelRegistryEntry {
    slot: KernelCell,
    last_used: u64,
}

impl IpythonSupervisor {
    pub(crate) fn new() -> Arc<Self> {
        Self::with_bridge(BridgeConfiguration {
            executable: configured_python(),
            source: BRIDGE_SOURCE,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Arc<Self> {
        Self::with_bridge(BridgeConfiguration {
            executable: configured_python(),
            source: TEST_BRIDGE_SOURCE,
        })
    }

    fn with_bridge(bridge: BridgeConfiguration) -> Arc<Self> {
        Arc::new(Self {
            kernels: Mutex::new(KernelRegistry {
                entries: HashMap::new(),
                usage_sequence: 0,
            }),
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_KERNELS)),
            stopping: AtomicBool::new(false),
            bridge: Arc::new(bridge),
        })
    }

    pub(crate) async fn execute(
        &self,
        session_id: SessionId,
        working_directory: PathBuf,
        input: &ToolInput,
        cancellation: &ProviderCancellation,
    ) -> ToolResult {
        let ToolInput::Ipython { cell } = input else {
            return ToolResult::error(ToolErrorKind::KernelUnavailable);
        };
        if self.stopping.load(Ordering::Acquire) {
            return ToolResult::error(ToolErrorKind::Interrupted);
        }
        if cancellation.is_cancelled() {
            return ToolResult::error(ToolErrorKind::Cancelled);
        }
        let slot = match self.kernel_slot(session_id).await {
            Ok(slot) => slot,
            Err(error) => return ToolResult::error(error),
        };
        let bridge = Arc::clone(&self.bridge);
        let startup_cancellation = cancellation.clone();
        let kernel = match slot
            .get_or_try_init(|| async move {
                let permit = self.reserve_kernel(session_id).await?;
                tokio::task::spawn_blocking(move || {
                    KernelProcess::start(working_directory, permit, bridge, &startup_cancellation)
                })
                .await
                .map_err(|_| ToolErrorKind::KernelUnavailable)?
                .map(|kernel| Arc::new(StdMutex::new(kernel)))
            })
            .await
        {
            Ok(kernel) => Arc::clone(kernel),
            Err(error) => {
                self.remove_slot(session_id, &slot).await;
                return ToolResult::error(error);
            }
        };
        let cell = cell.clone();
        let execution_cancellation = cancellation.clone();
        let execution = tokio::task::spawn_blocking(move || {
            kernel.lock().map_or_else(
                |_| KernelExecution::terminal(ToolResult::error(ToolErrorKind::Uncertain)),
                |mut kernel| kernel.execute(&cell, &execution_cancellation),
            )
        })
        .await
        .unwrap_or_else(|_| KernelExecution::terminal(ToolResult::error(ToolErrorKind::Uncertain)));
        if !execution.reusable {
            self.remove_slot(session_id, &slot).await;
        }
        execution.result
    }

    async fn kernel_slot(&self, session_id: SessionId) -> Result<KernelCell, ToolErrorKind> {
        let mut kernels = self.kernels.lock().await;
        let usage_sequence = kernels
            .usage_sequence
            .checked_add(1)
            .ok_or(ToolErrorKind::ResourceLimit)?;
        kernels.usage_sequence = usage_sequence;
        let entry = kernels
            .entries
            .entry(session_id)
            .or_insert_with(|| KernelRegistryEntry {
                slot: Arc::new(OnceCell::new()),
                last_used: usage_sequence,
            });
        entry.last_used = usage_sequence;
        Ok(Arc::clone(&entry.slot))
    }

    async fn reserve_kernel(
        &self,
        current_session: SessionId,
    ) -> Result<OwnedSemaphorePermit, ToolErrorKind> {
        if let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() {
            return Ok(permit);
        }
        let candidate = {
            let mut kernels = self.kernels.lock().await;
            let candidate = kernels
                .entries
                .iter()
                .filter(|(session_id, entry)| {
                    **session_id != current_session
                        && Arc::strong_count(&entry.slot) == 1
                        && entry
                            .slot
                            .get()
                            .is_some_and(|kernel| Arc::strong_count(kernel) == 1)
                })
                .min_by(|(left_id, left), (right_id, right)| {
                    left.last_used
                        .cmp(&right.last_used)
                        .then_with(|| left_id.as_bytes().cmp(right_id.as_bytes()))
                })
                .map(|(session_id, _)| *session_id);
            candidate.and_then(|session_id| {
                kernels
                    .entries
                    .remove(&session_id)
                    .and_then(|entry| entry.slot.get().cloned())
            })
        };
        let Some(candidate) = candidate else {
            return Err(ToolErrorKind::ResourceLimit);
        };
        let stopped = tokio::task::spawn_blocking(move || {
            candidate.lock().is_ok_and(|mut kernel| kernel.stop(false))
        })
        .await
        .unwrap_or(false);
        if !stopped {
            return Err(ToolErrorKind::Uncertain);
        }
        Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| ToolErrorKind::ResourceLimit)
    }

    async fn remove_slot(&self, session_id: SessionId, slot: &KernelCell) {
        let mut kernels = self.kernels.lock().await;
        if kernels
            .entries
            .get(&session_id)
            .is_some_and(|current| Arc::ptr_eq(&current.slot, slot))
        {
            kernels.entries.remove(&session_id);
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let slots = self
            .kernels
            .lock()
            .await
            .entries
            .drain()
            .map(|(_, entry)| entry.slot)
            .collect::<Vec<_>>();
        let mut tasks = Vec::new();
        for slot in slots {
            let Some(kernel) = slot.get().cloned() else {
                continue;
            };
            tasks.push(tokio::task::spawn_blocking(move || {
                kernel.lock().is_ok_and(|mut kernel| kernel.stop(false))
            }));
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

struct KernelExecution {
    result: ToolResult,
    reusable: bool,
}

impl KernelExecution {
    const fn reusable(result: ToolResult) -> Self {
        Self {
            result,
            reusable: true,
        }
    }

    const fn terminal(result: ToolResult) -> Self {
        Self {
            result,
            reusable: false,
        }
    }
}

struct KernelProcess {
    child: Option<Child>,
    process_group: u32,
    #[cfg(windows)]
    job: PlatformJob,
    #[cfg(unix)]
    kernel_process_group: Option<u32>,
    stdin: Option<ChildStdin>,
    messages: Receiver<BridgeMessage>,
    reader_failed: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<()>>,
    next_request_id: u64,
    _permit: OwnedSemaphorePermit,
}

impl KernelProcess {
    fn start(
        working_directory: PathBuf,
        permit: OwnedSemaphorePermit,
        bridge: Arc<BridgeConfiguration>,
        cancellation: &ProviderCancellation,
    ) -> Result<Self, ToolErrorKind> {
        let mut command = Command::new(&bridge.executable);
        command
            .arg("-u")
            .arg("-c")
            .arg(bridge.source)
            .current_dir(working_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        #[cfg(windows)]
        let job = fence_windows::KillOnCloseJob::new()
            .map(Some)
            .map_err(|_| ToolErrorKind::KernelUnavailable)?;
        #[cfg(not(windows))]
        let job = ();
        let mut child = command
            .spawn()
            .map_err(|_| ToolErrorKind::KernelUnavailable)?;
        #[cfg(windows)]
        if job.as_ref().is_none_or(|job| job.assign(&child).is_err()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ToolErrorKind::Uncertain);
        }
        let process_group = child.id();
        let Some(stdin) = child.stdin.take() else {
            let _ = terminate_tree(&mut child, process_group, false, job);
            return Err(ToolErrorKind::KernelUnavailable);
        };
        let Some(stdout) = child.stdout.take() else {
            drop(stdin);
            let _ = terminate_tree(&mut child, process_group, false, job);
            return Err(ToolErrorKind::KernelUnavailable);
        };
        let (sender, messages) = mpsc::sync_channel(BRIDGE_CHANNEL_CAPACITY);
        let reader_failed = Arc::new(AtomicBool::new(false));
        let reader = spawn_bridge_reader(stdout, sender, Arc::clone(&reader_failed));
        let mut kernel = Self {
            child: Some(child),
            process_group,
            #[cfg(windows)]
            job,
            #[cfg(unix)]
            kernel_process_group: None,
            stdin: Some(stdin),
            messages,
            reader_failed,
            reader: Some(reader),
            next_request_id: 1,
            _permit: permit,
        };
        match kernel.wait_until_ready(cancellation) {
            Ok(kernel_process_group) => {
                kernel.set_kernel_process_group(kernel_process_group);
                Ok(kernel)
            }
            Err(error) => {
                let _ = kernel.stop(false);
                Err(error)
            }
        }
    }

    #[cfg(unix)]
    fn set_kernel_process_group(&mut self, kernel_process_group: Option<u32>) {
        self.kernel_process_group = kernel_process_group;
    }

    #[cfg(windows)]
    fn set_kernel_process_group(&mut self, _kernel_process_group: Option<u32>) {}

    fn wait_until_ready(
        &mut self,
        cancellation: &ProviderCancellation,
    ) -> Result<Option<u32>, ToolErrorKind> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if cancellation.is_cancelled() {
                return Err(ToolErrorKind::Cancelled);
            }
            if self.reader_failed.load(Ordering::Acquire) {
                return Err(ToolErrorKind::KernelUnavailable);
            }
            if Instant::now() >= deadline {
                return Err(ToolErrorKind::TimedOut);
            }
            match self.messages.recv_timeout(POLL_INTERVAL) {
                Ok(BridgeMessage::Ready {
                    kernel_process_group,
                }) => {
                    #[cfg(unix)]
                    if kernel_process_group.is_none_or(|group| group == 0) {
                        return Err(ToolErrorKind::KernelUnavailable);
                    }
                    return Ok(kernel_process_group);
                }
                Ok(_) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(ToolErrorKind::KernelUnavailable);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            match self.child.as_mut().and_then(|child| child.try_wait().ok()) {
                Some(Some(_)) => return Err(ToolErrorKind::KernelUnavailable),
                Some(None) => {}
                None => return Err(ToolErrorKind::KernelUnavailable),
            }
        }
    }

    fn execute(&mut self, cell: &str, cancellation: &ProviderCancellation) -> KernelExecution {
        if cancellation.is_cancelled() {
            return self.stop_for(ToolErrorKind::Cancelled, CellOutput::default());
        }
        let request_id = self.next_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            return self.stop_for(ToolErrorKind::ResourceLimit, CellOutput::default());
        };
        self.next_request_id = next_request_id;
        if self
            .send(&BridgeRequest::Execute {
                id: request_id,
                code: cell,
            })
            .is_err()
        {
            return self.stop_for(ToolErrorKind::Uncertain, CellOutput::default());
        }

        let deadline = Instant::now() + CELL_WALL_TIME_LIMIT;
        let mut last_activity = Instant::now();
        let mut output = CellOutput::default();
        loop {
            if cancellation.is_cancelled() {
                return self.stop_for(ToolErrorKind::Cancelled, output);
            }
            if self.reader_failed.load(Ordering::Acquire) {
                return self.stop_for(ToolErrorKind::Uncertain, output);
            }
            let now = Instant::now();
            if now >= deadline {
                return self.stop_for(ToolErrorKind::TimedOut, output);
            }
            if now.duration_since(last_activity) >= CELL_INACTIVITY_LIMIT {
                return self.stop_for(ToolErrorKind::InactivityTimeout, output);
            }
            match self.messages.recv_timeout(POLL_INTERVAL) {
                Ok(BridgeMessage::Output { id, channel, text }) if id == request_id => {
                    last_activity = Instant::now();
                    if !output.push(channel, &text) {
                        return self.stop_for(ToolErrorKind::OutputLimit, output);
                    }
                }
                Ok(BridgeMessage::Complete {
                    id,
                    status,
                    execution_count,
                }) if id == request_id => {
                    let tool_output = output.finish(execution_count);
                    return match status {
                        BridgeStatus::Ok => KernelExecution::reusable(ToolResult::Ok {
                            output: tool_output,
                        }),
                        BridgeStatus::Error => {
                            KernelExecution::reusable(ToolResult::error_with_output(
                                ToolErrorKind::ExecutionFailed,
                                tool_output,
                            ))
                        }
                    };
                }
                Ok(_) | Err(RecvTimeoutError::Disconnected) => {
                    return self.stop_for(ToolErrorKind::Uncertain, output);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            match self.child.as_mut().and_then(|child| child.try_wait().ok()) {
                Some(Some(_)) | None => {
                    return self.stop_for(ToolErrorKind::Uncertain, output);
                }
                Some(None) => {}
            }
        }
    }

    fn send(&mut self, request: &BridgeRequest<'_>) -> Result<(), ()> {
        let stdin = self.stdin.as_mut().ok_or(())?;
        serde_json::to_writer(&mut *stdin, request).map_err(|_| ())?;
        stdin.write_all(b"\n").map_err(|_| ())?;
        stdin.flush().map_err(|_| ())
    }

    fn stop_for(&mut self, error: ToolErrorKind, output: CellOutput) -> KernelExecution {
        let result = if self.stop(false) {
            ToolResult::error_with_output(error, output.finish(None))
        } else {
            ToolResult::error_with_output(ToolErrorKind::Uncertain, output.finish(None))
        };
        KernelExecution::terminal(result)
    }

    fn stop(&mut self, already_reaped: bool) -> bool {
        self.stdin.take();
        #[cfg(unix)]
        let kernel_stopped = self
            .kernel_process_group
            .take()
            .is_none_or(|group| group == self.process_group || terminate_process_group(group));
        #[cfg(windows)]
        let kernel_stopped = true;
        let bridge_stopped = self.child.take().is_none_or(|mut child| {
            #[cfg(windows)]
            let job = std::mem::take(&mut self.job);
            #[cfg(not(windows))]
            let job = ();
            terminate_tree(&mut child, self.process_group, already_reaped, job)
        });
        let stopped = kernel_stopped && bridge_stopped;
        if stopped {
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        } else {
            self.reader.take();
        }
        stopped
    }
}

impl Drop for KernelProcess {
    fn drop(&mut self) {
        let _ = self.stop(false);
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeRequest<'a> {
    Execute { id: u64, code: &'a str },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BridgeMessage {
    Ready {
        kernel_process_group: Option<u32>,
    },
    Output {
        id: u64,
        channel: BridgeChannel,
        text: String,
    },
    Complete {
        id: u64,
        status: BridgeStatus,
        execution_count: Option<u32>,
    },
    ProtocolError {},
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BridgeChannel {
    Stdout,
    Stderr,
    Display,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BridgeStatus {
    Ok,
    Error,
}

#[derive(Default)]
struct CellOutput {
    stdout: String,
    stderr: String,
    display: String,
    bytes: usize,
}

impl CellOutput {
    fn push(&mut self, channel: BridgeChannel, text: &str) -> bool {
        let remaining = MAX_IPYTHON_OUTPUT_BYTES.saturating_sub(self.bytes);
        if text.len() > remaining {
            let target = match channel {
                BridgeChannel::Stdout => &mut self.stdout,
                BridgeChannel::Stderr => &mut self.stderr,
                BridgeChannel::Display => &mut self.display,
            };
            let mut boundary = remaining;
            while !text.is_char_boundary(boundary) {
                boundary = boundary.saturating_sub(1);
            }
            target.push_str(&text[..boundary]);
            self.bytes = MAX_IPYTHON_OUTPUT_BYTES;
            return false;
        }
        match channel {
            BridgeChannel::Stdout => self.stdout.push_str(text),
            BridgeChannel::Stderr => self.stderr.push_str(text),
            BridgeChannel::Display => self.display.push_str(text),
        }
        self.bytes += text.len();
        true
    }

    fn finish(self, execution_count: Option<u32>) -> ToolOutput {
        ToolOutput::Ipython {
            execution_count,
            stdout: self.stdout,
            stderr: self.stderr,
            display: self.display,
        }
    }
}

fn configured_python() -> std::ffi::OsString {
    if let Some(configured) = std::env::var_os("MORONS_PYTHON").filter(|value| !value.is_empty()) {
        return configured;
    }
    #[cfg(windows)]
    return "python".into();
    #[cfg(not(windows))]
    "python3".into()
}

fn spawn_bridge_reader<R: Read + Send + 'static>(
    mut reader: R,
    sender: SyncSender<BridgeMessage>,
    failed: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut pending = Vec::new();
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let bytes = match reader.read(&mut buffer) {
                Ok(0) => {
                    failed.store(true, Ordering::Release);
                    return;
                }
                Ok(bytes) => bytes,
                Err(_) => {
                    failed.store(true, Ordering::Release);
                    return;
                }
            };
            for byte in &buffer[..bytes] {
                if *byte == b'\n' {
                    let message = serde_json::from_slice::<BridgeMessage>(&pending);
                    pending.clear();
                    let Ok(message) = message else {
                        failed.store(true, Ordering::Release);
                        return;
                    };
                    match sender.try_send(message) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                            failed.store(true, Ordering::Release);
                            return;
                        }
                    }
                } else {
                    if pending.len() == MAX_BRIDGE_LINE_BYTES {
                        failed.store(true, Ordering::Release);
                        return;
                    }
                    pending.push(*byte);
                }
            }
        }
    })
}

pub(crate) fn validate_ipython_cell(cell: &str) -> bool {
    !cell.is_empty() && cell.len() <= MAX_IPYTHON_CELL_BYTES && !cell.contains('\0')
}

#[cfg(test)]
mod tests {
    use tokio::time;

    use super::*;

    #[test]
    fn bridge_messages_are_strict_and_cell_output_is_aggregate_bounded() {
        let ready: BridgeMessage =
            serde_json::from_slice(br#"{"type":"ready","kernel_process_group":123}"#)
                .expect("ready should decode");
        assert!(matches!(
            ready,
            BridgeMessage::Ready {
                kernel_process_group: Some(123)
            }
        ));
        assert!(
            serde_json::from_slice::<BridgeMessage>(
                br#"{"type":"ready","kernel_process_group":123,"unexpected":true}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_slice::<BridgeMessage>(
                br#"{"type":"output","id":1,"channel":"stdout","text":"a","text":"b"}"#
            )
            .is_err()
        );
        let mut output = CellOutput::default();
        assert!(!output.push(
            BridgeChannel::Stdout,
            &"x".repeat(MAX_IPYTHON_OUTPUT_BYTES + 1)
        ));
        let ToolOutput::Ipython { stdout, .. } = output.finish(None) else {
            panic!("cell output should remain typed");
        };
        assert_eq!(stdout.len(), MAX_IPYTHON_OUTPUT_BYTES);
    }

    #[test]
    fn bridge_reader_rejects_oversized_or_malformed_records() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let failed = Arc::new(AtomicBool::new(false));
        let reader = spawn_bridge_reader(&b"{not json}\n"[..], sender, Arc::clone(&failed));
        reader.join().expect("reader should join");
        assert!(failed.load(Ordering::Acquire));
        assert!(receiver.try_recv().is_err());

        let (sender, _) = mpsc::sync_channel(2);
        let failed = Arc::new(AtomicBool::new(false));
        let reader = spawn_bridge_reader(
            std::io::Cursor::new(vec![b'x'; MAX_BRIDGE_LINE_BYTES + 1]),
            sender,
            Arc::clone(&failed),
        );
        reader.join().expect("reader should join");
        assert!(failed.load(Ordering::Acquire));
    }

    #[test]
    fn cells_are_bounded_without_restricting_normal_python_source() {
        assert!(validate_ipython_cell("value = 1\nvalue + 1"));
        assert!(!validate_ipython_cell(""));
        assert!(!validate_ipython_cell("bad\0cell"));
        assert!(!validate_ipython_cell(
            &"x".repeat(MAX_IPYTHON_CELL_BYTES + 1)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_kernels_preserve_memory_and_selected_working_directory() {
        let root = test_directory("persistent");
        let supervisor = IpythonSupervisor::for_test();
        let session = SessionId::from_bytes([0x41; 16]);
        let (_, cancellation) = crate::provider::provider_cancellation();
        let assigned = supervisor
            .execute(
                session,
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value = 41".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            assigned,
            ToolResult::Ok {
                output: ToolOutput::Ipython {
                    execution_count: Some(1),
                    ..
                }
            }
        ));
        let recalled = supervisor
            .execute(
                session,
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value + 1".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            recalled,
            ToolResult::Ok {
                output: ToolOutput::Ipython {
                    execution_count: Some(2),
                    ref display,
                    ..
                }
            } if display == "42"
        ));
        let cwd = supervisor
            .execute(
                session,
                root.clone(),
                &ToolInput::Ipython {
                    cell: "cwd".to_owned(),
                },
                &cancellation,
            )
            .await;
        let ToolResult::Ok {
            output: ToolOutput::Ipython { stdout, .. },
        } = cwd
        else {
            panic!("working-directory cell should succeed");
        };
        assert_eq!(
            PathBuf::from(stdout)
                .canonicalize()
                .expect("reported working directory should resolve"),
            root.canonicalize()
                .expect("selected working directory should resolve")
        );
        let other = supervisor
            .execute(
                SessionId::from_bytes([0x42; 16]),
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value + 1".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            other,
            ToolResult::Error {
                error: ToolErrorKind::ExecutionFailed,
                output: Some(ToolOutput::Ipython { ref stderr, .. }),
            } if stderr.contains("NameError")
        ));
        supervisor.shutdown().await;
        std::fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn least_recently_used_idle_kernel_is_evicted_at_capacity() {
        let root = test_directory("eviction");
        let supervisor = IpythonSupervisor::for_test();
        let (_, cancellation) = crate::provider::provider_cancellation();
        for marker in 1_u8..=5 {
            let result = supervisor
                .execute(
                    SessionId::from_bytes([marker; 16]),
                    root.clone(),
                    &ToolInput::Ipython {
                        cell: "value = 41".to_owned(),
                    },
                    &cancellation,
                )
                .await;
            assert!(matches!(result, ToolResult::Ok { .. }));
        }
        let first = supervisor
            .execute(
                SessionId::from_bytes([1; 16]),
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value + 1".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            first,
            ToolResult::Error {
                error: ToolErrorKind::ExecutionFailed,
                output: Some(ToolOutput::Ipython {
                    execution_count: Some(1),
                    ..
                }),
            }
        ));
        supervisor.shutdown().await;
        std::fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Python with jupyter_client and ipykernel installed"]
    async fn live_standard_jupyter_bridge_preserves_ipython_state() {
        let root = test_directory("live-jupyter");
        let supervisor = IpythonSupervisor::new();
        let session = SessionId::from_bytes([0x44; 16]);
        let (_, cancellation) = crate::provider::provider_cancellation();
        let first = supervisor
            .execute(
                session,
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value = 21\nprint('ready')\nvalue + 1".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            first,
            ToolResult::Ok {
                output: ToolOutput::Ipython {
                    ref stdout,
                    ref display,
                    ..
                }
            } if stdout == "ready\n" && display == "22"
        ));
        let second = supervisor
            .execute(
                session,
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value * 2".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            second,
            ToolResult::Ok {
                output: ToolOutput::Ipython { ref display, .. }
            } if display == "42"
        ));
        supervisor.shutdown().await;
        std::fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires Python with jupyter_client and ipykernel installed"]
    async fn live_jupyter_cancellation_stops_kernel_descendants() {
        let root = test_directory("live-cancellation");
        let started = root.join("started");
        let leaked = root.join("leaked");
        let supervisor = IpythonSupervisor::new();
        let session = SessionId::from_bytes([0x45; 16]);
        let (handle, cancellation) = crate::provider::provider_cancellation();
        let execution_supervisor = Arc::clone(&supervisor);
        let execution_root = root.clone();
        let started_literal =
            serde_json::to_string(&started.to_string_lossy()).expect("started path should encode");
        let leaked_literal =
            serde_json::to_string(&leaked.to_string_lossy()).expect("leaked path should encode");
        let cell = format!(
            "from pathlib import Path\nimport subprocess, sys, time\nPath({started_literal}).write_text('started')\nsubprocess.Popen([sys.executable, '-c', \"import pathlib,sys,time;time.sleep(2);pathlib.Path(sys.argv[1]).write_text('leaked')\", {leaked_literal}])\ntime.sleep(60)"
        );
        let execution = tokio::spawn(async move {
            execution_supervisor
                .execute(
                    session,
                    execution_root,
                    &ToolInput::Ipython { cell },
                    &cancellation,
                )
                .await
        });
        time::timeout(Duration::from_secs(30), async {
            while !started.exists() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("live cell should start");
        handle.cancel();
        let result = execution.await.expect("cell task should join");
        assert!(matches!(
            result,
            ToolResult::Error {
                error: ToolErrorKind::Cancelled,
                ..
            }
        ));
        time::sleep(Duration::from_millis(2_300)).await;
        assert!(!leaked.exists());
        supervisor.shutdown().await;
        std::fs::remove_dir_all(root).expect("test directory should be removed");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_terminates_the_kernel_and_its_descendants_before_restart() {
        let root = test_directory("cancellation");
        let started = root.join("started");
        let leaked = root.join("leaked");
        let supervisor = IpythonSupervisor::for_test();
        let session = SessionId::from_bytes([0x43; 16]);
        let (handle, cancellation) = crate::provider::provider_cancellation();
        let execution_supervisor = Arc::clone(&supervisor);
        let execution_root = root.clone();
        let cell = format!(
            "spawn:{}|{}",
            started.to_string_lossy(),
            leaked.to_string_lossy()
        );
        let execution = tokio::spawn(async move {
            execution_supervisor
                .execute(
                    session,
                    execution_root,
                    &ToolInput::Ipython { cell },
                    &cancellation,
                )
                .await
        });
        time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake cell should spawn its descendant");
        handle.cancel();
        assert!(matches!(
            execution.await.expect("cell task should join"),
            ToolResult::Error {
                error: ToolErrorKind::Cancelled,
                ..
            }
        ));
        time::sleep(Duration::from_millis(2_300)).await;
        assert!(!leaked.exists());

        let (_, cancellation) = crate::provider::provider_cancellation();
        let restarted = supervisor
            .execute(
                session,
                root.clone(),
                &ToolInput::Ipython {
                    cell: "value = 41".to_owned(),
                },
                &cancellation,
            )
            .await;
        assert!(matches!(
            restarted,
            ToolResult::Ok {
                output: ToolOutput::Ipython {
                    execution_count: Some(1),
                    ..
                }
            }
        ));
        supervisor.shutdown().await;
        std::fs::remove_dir_all(root).expect("test directory should be removed");
    }

    fn test_directory(label: &str) -> PathBuf {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("test randomness should be available");
        let encoded = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!(
            "morons-ipython-{label}-{}-{encoded}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("test directory should be created");
        path
    }
}
