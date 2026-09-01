use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::ExitStatusExt},
    path::Path,
    process::{Child, ExitStatus},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use rustix::{
    io::Errno,
    process::{Pid, Signal, WaitId, WaitIdOptions, WaitOptions, kill_process_group},
};
use serde::{Deserialize, Serialize};

use crate::{SANDBOX_PROTOCOL_VERSION, SandboxResult, SandboxStatus, linux::platform::Layout};

pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TREE_TERMINATION_TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const MAX_SANDBOX_TASKS: usize = 64;
const MAX_STAGE_RECORD_BYTES: usize = 1024;
pub(super) const READY_BYTES: &[u8] = b"ready\n";
pub(super) const RESOURCE_LIMIT_BYTES: &[u8] = b"resource_limit\n";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StageOutcome {
    protocol_version: u16,
    operation_id: [u8; 16],
    pub(super) code: Option<i32>,
    pub(super) signal: Option<u16>,
}

impl StageOutcome {
    fn from_status(operation_id: [u8; 16], status: ExitStatus) -> Result<Self, ()> {
        let outcome = Self {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id,
            code: status.code(),
            signal: status
                .signal()
                .and_then(|signal| u16::try_from(signal).ok()),
        };
        if outcome.valid_for(operation_id) {
            Ok(outcome)
        } else {
            Err(())
        }
    }

    fn valid_for(&self, operation_id: [u8; 16]) -> bool {
        self.protocol_version == SANDBOX_PROTOCOL_VERSION
            && self.operation_id == operation_id
            && (self.code.is_some() ^ self.signal.is_some())
    }
}

pub(super) fn child_exited_without_reaping(child: Pid) -> Result<bool, ()> {
    rustix::process::waitid(
        WaitId::Pid(child),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|status| status.is_some())
    .map_err(|_| ())
}

pub(super) fn terminate_inner_group(child: &mut Child, group: Pid) -> Result<ExitStatus, ()> {
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => {}
        Err(_) => return Err(()),
    }
    let deadline = Instant::now() + TREE_TERMINATION_TIMEOUT;
    loop {
        match process_group_has_live_members(group) {
            Ok(false) => break,
            Ok(true) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(true) | Err(()) => return Err(()),
        }
    }
    let status = child.wait().map_err(|_| ())?;
    loop {
        match rustix::process::wait(WaitOptions::NOHANG) {
            Ok(Some(_)) => {}
            Ok(None) | Err(Errno::CHILD) => break,
            Err(_) => return Err(()),
        }
    }
    if process_group_has_members(group)? {
        return Err(());
    }
    Ok(status)
}

pub(super) fn terminate_outer_group(child: &mut Child, group: Pid) -> bool {
    match kill_process_group(group, Signal::KILL) {
        Ok(()) | Err(Errno::SRCH) => {}
        Err(_) => return false,
    }
    let deadline = Instant::now() + TREE_TERMINATION_TIMEOUT;
    loop {
        match process_group_has_live_members(group) {
            Ok(false) => break,
            Ok(true) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(true) | Err(()) => return false,
        }
    }
    child.wait().is_ok()
}

fn process_group_has_live_members(group: Pid) -> Result<bool, ()> {
    process_group_states(group)
        .map(|states| states.into_iter().any(|state| state != 'Z' && state != 'X'))
}

fn process_group_has_members(group: Pid) -> Result<bool, ()> {
    process_group_states(group).map(|states| !states.is_empty())
}

fn process_group_states(group: Pid) -> Result<Vec<char>, ()> {
    let mut states = Vec::new();
    let group = group.as_raw_nonzero().get();
    for entry in fs::read_dir("/proc").map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let stat = match fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        if stat.len() > 4096 {
            return Err(());
        }
        let stat = std::str::from_utf8(&stat).map_err(|_| ())?;
        let close = stat.rfind(')').ok_or(())?;
        let mut fields = stat.get(close + 1..).ok_or(())?.split_ascii_whitespace();
        let state = fields
            .next()
            .and_then(|value| value.chars().next())
            .ok_or(())?;
        let _parent = fields.next().ok_or(())?.parse::<i32>().map_err(|_| ())?;
        let process_group = fields.next().ok_or(())?.parse::<i32>().map_err(|_| ())?;
        if process_group == group {
            states.push(state);
        }
    }
    Ok(states)
}

pub(super) fn namespace_task_count() -> Result<usize, ()> {
    let mut count = 0_usize;
    for entry in fs::read_dir("/proc").map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let tasks = match fs::read_dir(entry.path().join("task")) {
            Ok(tasks) => tasks,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        for task in tasks {
            task.map_err(|_| ())?;
            count = count.checked_add(1).ok_or(())?;
            if count > MAX_SANDBOX_TASKS {
                return Ok(count);
            }
        }
    }
    Ok(count)
}

pub(super) fn stop_outer_setup_failure(
    operation_id: [u8; 16],
    mut child: Child,
    layout: &Layout,
) -> SandboxResult {
    let group = Pid::from_raw(i32::try_from(child.id()).unwrap_or_default());
    let stopped = group.is_some_and(|group| terminate_outer_group(&mut child, group));
    if stopped {
        let _ = layout.cleanup();
    }
    SandboxResult::failure(
        operation_id,
        if stopped {
            SandboxStatus::LaunchFailed
        } else {
            SandboxStatus::ProcessTreeUncertain
        },
    )
}

pub(super) fn capture_stream<R: Read + Send + 'static>(
    mut reader: R,
    maximum: usize,
    exceeded: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut output = Vec::with_capacity(maximum.min(8 * 1024));
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => {
                    exceeded.store(true, Ordering::Release);
                    break;
                }
            };
            let Some(next) = output.len().checked_add(read) else {
                exceeded.store(true, Ordering::Release);
                continue;
            };
            if next > maximum {
                exceeded.store(true, Ordering::Release);
                continue;
            }
            output.extend_from_slice(&buffer[..read]);
        }
        output
    })
}

pub(super) fn join_stream(handle: thread::JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

pub(super) fn write_stage_marker(path: &str, bytes: &[u8]) -> Result<(), ()> {
    if bytes.is_empty() || bytes.len() > MAX_STAGE_RECORD_BYTES {
        return Err(());
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| ())?;
    file.write_all(bytes).map_err(|_| ())?;
    file.sync_all().map_err(|_| ())
}

pub(super) fn write_stage_outcome(
    path: &str,
    operation_id: [u8; 16],
    status: ExitStatus,
) -> Result<(), ()> {
    let outcome = StageOutcome::from_status(operation_id, status)?;
    let bytes = serde_json::to_vec(&outcome).map_err(|_| ())?;
    if bytes.len() > MAX_STAGE_RECORD_BYTES {
        return Err(());
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| ())?;
    file.write_all(&bytes).map_err(|_| ())?;
    file.sync_all().map_err(|_| ())
}

pub(super) fn read_stage_outcome(path: &Path, operation_id: [u8; 16]) -> Result<StageOutcome, ()> {
    let file = File::open(path).map_err(|_| ())?;
    let mut bytes = Vec::new();
    file.take((MAX_STAGE_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.is_empty() || bytes.len() > MAX_STAGE_RECORD_BYTES {
        return Err(());
    }
    let outcome: StageOutcome = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if !outcome.valid_for(operation_id) || serde_json::to_vec(&outcome).map_err(|_| ())? != bytes {
        return Err(());
    }
    Ok(outcome)
}
