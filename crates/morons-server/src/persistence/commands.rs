use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use morons_sandbox::{
    SANDBOX_PROTOCOL_VERSION, SandboxLimits, SandboxRequest, SandboxStatus, read_result,
    write_request,
};
use tokio::sync::oneshot;

use super::{
    CommandResources, CommittedToolCall, PersistenceError, RepositoryImportOutcome, RunId,
    SessionStore, TranscriptEntry, WorkerRequest, backend::command_execution::CommandBinding,
};
use crate::{
    provider::ProviderCancellation,
    tools::{ToolErrorKind, ToolInput, ToolOutput, ToolResult},
};

const COMMAND_WALL_TIME_MILLISECONDS: u64 = 10 * 60 * 1_000;
const COMMAND_OUTPUT_BYTES: u32 = 256 * 1024;
const HELPER_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_HELPER_STDERR_BYTES: usize = 16 * 1024;

impl SessionStore {
    pub(crate) async fn execute_command_call(
        self: &Arc<Self>,
        run_id: RunId,
        workspace_id: [u8; 16],
        call: &CommittedToolCall,
        cancellation: &ProviderCancellation,
    ) -> Result<TranscriptEntry, PersistenceError> {
        let ToolInput::RunCommand {
            executable,
            arguments,
            working_directory,
        } = &call.input
        else {
            return Err(PersistenceError::InvalidState {
                reason: "command execution received a non-command tool",
            });
        };
        let _image_guard = self.execution_image_lock.lock().await;
        let _workspace_guard = self.repository_import_lock.lock().await;
        let resources = self.command_resources(run_id, workspace_id).await?;
        let generation_id = random_identifier()?;
        let paths = self.paths.clone();
        let operation_id = *call.operation_id.as_bytes();
        let resources_for_copy = resources;
        let workspace = tokio::task::spawn_blocking(move || {
            paths.prepare_command_workspace(
                &resources_for_copy.workspace_id,
                &resources_for_copy.active_generation_id,
                &operation_id,
            )
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)??;
        let binding = match self
            .prepare_command(
                run_id,
                call,
                resources,
                generation_id,
                workspace.source_outcome,
            )
            .await
        {
            Ok(binding) => binding,
            Err(error) => {
                let paths = self.paths.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    paths.remove_command_operation(&operation_id)
                })
                .await;
                return Err(error);
            }
        };
        if cancellation.is_cancelled() {
            let result = ToolResult::error(ToolErrorKind::Cancelled);
            let entry = self.complete_command(run_id, call, result, None).await?;
            self.cleanup_command(operation_id, None, None, workspace_id)
                .await?;
            return Ok(entry);
        }
        self.mark_tool_dispatched(run_id, call.call_id, call.operation_id)
            .await?;
        let request = SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id,
            candidate_root: host_path(&workspace.candidate)?,
            scratch_root: host_path(&workspace.scratch)?,
            image_root: host_path(
                &self
                    .paths
                    .execution_image_path(&binding.image_generation_id),
            )?,
            executable: image_executable(executable),
            arguments: arguments.clone(),
            working_directory: working_directory.as_str().to_owned(),
            limits: SandboxLimits {
                wall_time_milliseconds: COMMAND_WALL_TIME_MILLISECONDS,
                output_bytes_per_stream: COMMAND_OUTPUT_BYTES,
            },
        };
        let helper = sandbox_helper_path().map_err(|()| PersistenceError::InvalidState {
            reason: "the packaged sandbox helper is unavailable",
        })?;
        let cancellation = cancellation.clone();
        let sandbox =
            tokio::task::spawn_blocking(move || run_helper(&helper, request, &cancellation))
                .await
                .map_err(|_| PersistenceError::WorkerStopped)?;
        let (result, publication, unreferenced) = match sandbox {
            Ok(result) if result.status == SandboxStatus::Exited && result.candidate_eligible => {
                let paths = self.paths.clone();
                let candidate = workspace.candidate.clone();
                let binding_for_copy = binding.clone();
                match tokio::task::spawn_blocking(move || {
                    paths.publish_command_generation(
                        &binding_for_copy.workspace_id,
                        &binding_for_copy.generation_id,
                        &operation_id,
                        &candidate,
                    )
                })
                .await
                .map_err(|_| PersistenceError::WorkerStopped)?
                {
                    Ok(outcome) => (
                        command_result(
                            executable,
                            &result,
                            true,
                            &workspace.candidate,
                            &workspace.scratch,
                            &self
                                .paths
                                .execution_image_path(&binding.image_generation_id),
                        ),
                        Some((binding, outcome)),
                        None,
                    ),
                    Err(_) => (
                        ToolResult::error(ToolErrorKind::Filesystem),
                        None,
                        Some(generation_id),
                    ),
                }
            }
            Ok(result) => (sandbox_failure(&result), None, None),
            Err(_) => (ToolResult::error(ToolErrorKind::Interrupted), None, None),
        };
        let obsolete = publication
            .as_ref()
            .map(|(binding, _)| binding.active_generation_id);
        let entry = self
            .complete_command(run_id, call, result, publication)
            .await?;
        self.cleanup_command(operation_id, unreferenced, obsolete, workspace_id)
            .await?;
        Ok(entry)
    }

    async fn command_resources(
        &self,
        run_id: RunId,
        workspace_id: [u8; 16],
    ) -> Result<CommandResources, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::GetCommandResources {
                run_id,
                workspace_id,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn prepare_command(
        &self,
        run_id: RunId,
        call: &CommittedToolCall,
        resources: CommandResources,
        generation_id: [u8; 16],
        source: RepositoryImportOutcome,
    ) -> Result<CommandBinding, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::PrepareCommandOperation {
                run_id,
                call_id: call.call_id,
                operation_id: call.operation_id,
                resources,
                generation_id,
                source,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn complete_command(
        &self,
        run_id: RunId,
        call: &CommittedToolCall,
        result: ToolResult,
        publication: Option<(CommandBinding, RepositoryImportOutcome)>,
    ) -> Result<TranscriptEntry, PersistenceError> {
        let (response, receiver) = oneshot::channel();
        self.sender()?
            .send(WorkerRequest::CompleteCommandResult {
                run_id,
                call_id: call.call_id,
                operation_id: call.operation_id,
                result,
                publication,
                response,
            })
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?;
        receiver
            .await
            .map_err(|_| PersistenceError::WorkerStopped)?
    }

    async fn cleanup_command(
        &self,
        operation_id: [u8; 16],
        generation_id: Option<[u8; 16]>,
        obsolete_generation_id: Option<[u8; 16]>,
        workspace_id: [u8; 16],
    ) -> Result<(), PersistenceError> {
        let paths = self.paths.clone();
        tokio::task::spawn_blocking(move || {
            paths.remove_command_operation(&operation_id)?;
            for generation_id in [generation_id, obsolete_generation_id]
                .into_iter()
                .flatten()
            {
                paths.remove_unreferenced_generation(&workspace_id, &generation_id)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| PersistenceError::WorkerStopped)?
    }
}

fn run_helper(
    helper: &Path,
    request: SandboxRequest,
    cancellation: &ProviderCancellation,
) -> Result<morons_sandbox::SandboxResult, ()> {
    let mut child = Command::new(helper)
        .current_dir(root_directory())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| ())?;
    let mut input = child.stdin.take().ok_or(())?;
    write_request(&mut input, &request).map_err(|_| ())?;
    let mut input = Some(input);
    let stdout = child.stdout.take().ok_or(())?;
    let stderr = child.stderr.take().ok_or(())?;
    let result_reader = thread::spawn(move || read_result(&mut std::io::BufReader::new(stdout)));
    let stderr_exceeded = Arc::new(AtomicBool::new(false));
    let exceeded = Arc::clone(&stderr_exceeded);
    let stderr_reader =
        thread::spawn(move || read_bounded(stderr, MAX_HELPER_STDERR_BYTES, exceeded));
    let deadline = Instant::now() + Duration::from_millis(COMMAND_WALL_TIME_MILLISECONDS + 10_000);
    let status = 'wait: loop {
        if cancellation.is_cancelled() || Instant::now() >= deadline {
            drop(input.take());
            let stop = Instant::now() + HELPER_STOP_TIMEOUT;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => break 'wait status,
                    Ok(None) if Instant::now() < stop => thread::sleep(POLL_INTERVAL),
                    Ok(None) | Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(());
                    }
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(_) => return Err(()),
        }
    };
    drop(input.take());
    let result = result_reader.join().map_err(|_| ())?.map_err(|_| ())?;
    let stderr = stderr_reader.join().map_err(|_| ())?;
    if !status.success() || stderr_exceeded.load(Ordering::Acquire) || !stderr.is_empty() {
        return Err(());
    }
    Ok(result)
}

fn read_bounded(mut reader: impl Read, maximum: usize, exceeded: Arc<AtomicBool>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) if output.len().saturating_add(read) <= maximum => {
                output.extend_from_slice(&buffer[..read]);
            }
            Ok(_) | Err(_) => {
                exceeded.store(true, Ordering::Release);
                break;
            }
        }
    }
    output
}

fn command_result(
    executable: &str,
    result: &morons_sandbox::SandboxResult,
    published: bool,
    candidate: &Path,
    scratch: &Path,
    image: &Path,
) -> ToolResult {
    ToolResult::Ok {
        output: ToolOutput::CommandCompleted {
            executable: executable.to_owned(),
            exit_code: result.exit.and_then(|exit| exit.code).unwrap_or(-1),
            stdout: sanitize_output(&result.stdout, candidate, scratch, image),
            stderr: sanitize_output(&result.stderr, candidate, scratch, image),
            published,
        },
    }
}

fn sandbox_failure(result: &morons_sandbox::SandboxResult) -> ToolResult {
    let error = match result.status {
        SandboxStatus::Cancelled => ToolErrorKind::Cancelled,
        SandboxStatus::TimedOut | SandboxStatus::OutputLimit | SandboxStatus::ResourceLimit => {
            ToolErrorKind::ResourceLimit
        }
        SandboxStatus::RequestRejected
        | SandboxStatus::BackendUnavailable
        | SandboxStatus::LaunchFailed
        | SandboxStatus::ProcessTreeUncertain
        | SandboxStatus::Signalled
        | SandboxStatus::Crashed
        | SandboxStatus::Exited => ToolErrorKind::Interrupted,
    };
    ToolResult::error(error)
}

fn sanitize_output(bytes: &[u8], candidate: &Path, scratch: &Path, image: &Path) -> String {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes
            .iter()
            .take(crate::tools::MAX_COMMAND_OUTPUT_BYTES / 4)
            .map(|byte| format!("\\x{byte:02x}"))
            .collect();
    };
    let sanitized: String = text
        .chars()
        .filter(|character| {
            *character == '\n'
                || *character == '\t'
                || !character.is_control()
                    && !matches!(
                        character,
                        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}'
                            | '\u{2066}'..='\u{2069}'
                    )
        })
        .collect();
    [
        (candidate, "/workspace"),
        (scratch, "/scratch"),
        (image, "/image"),
    ]
    .into_iter()
    .fold(sanitized, |text, (path, replacement)| {
        path.to_str()
            .map_or(text.clone(), |path| text.replace(path, replacement))
    })
}

fn sandbox_helper_path() -> Result<PathBuf, ()> {
    let executable = fs::canonicalize(std::env::current_exe().map_err(|_| ())?).map_err(|_| ())?;
    let name = if cfg!(windows) {
        "morons-sandbox.exe"
    } else {
        "morons-sandbox"
    };
    let parent = executable.parent().ok_or(())?;
    let direct = parent.join(name);
    let helper = if direct.is_file() {
        direct
    } else if parent.file_name() == Some(std::ffi::OsStr::new("deps")) {
        parent.parent().ok_or(())?.join(name)
    } else {
        return Err(());
    };
    let metadata = fs::symlink_metadata(&helper).map_err(|_| ())?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(());
    }
    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(());
    }
    fs::canonicalize(helper).map_err(|_| ())
}

fn host_path(path: &Path) -> Result<String, PersistenceError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(PersistenceError::InvalidState {
            reason: "a command staging path is not UTF-8",
        })
}

fn image_executable(executable: &str) -> String {
    if cfg!(windows) {
        format!("bin/{executable}.exe")
    } else {
        format!("bin/{executable}")
    }
}

fn root_directory() -> &'static Path {
    if cfg!(windows) {
        Path::new(r"C:\")
    } else {
        Path::new("/")
    }
}

fn random_identifier() -> Result<[u8; 16], PersistenceError> {
    let mut identifier = [0_u8; 16];
    getrandom::fill(&mut identifier)?;
    if identifier.iter().all(|byte| *byte == 0) {
        return Err(PersistenceError::InvalidState {
            reason: "command generation randomness was invalid",
        });
    }
    Ok(identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_output_is_bounded_terminal_safe_and_path_mapped() {
        let candidate = Path::new("/private/candidate");
        let scratch = Path::new("/private/scratch");
        let image = Path::new("/private/image");
        let output = sanitize_output(
            b"/private/candidate/file\x1b]52;c;bad\x07\xe2\x80\xae\n",
            candidate,
            scratch,
            image,
        );
        assert!(output.starts_with("/workspace/file]52;c;bad"));
        assert!(!output.contains('\u{1b}'));
        assert!(!output.contains('\u{7}'));
        assert!(!output.contains('\u{202e}'));

        let invalid = sanitize_output(
            &vec![0xff; crate::tools::MAX_COMMAND_OUTPUT_BYTES],
            candidate,
            scratch,
            image,
        );
        assert_eq!(invalid.len(), crate::tools::MAX_COMMAND_OUTPUT_BYTES);
        assert!(invalid.starts_with("\\xff"));
    }
}
