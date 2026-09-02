use std::{
    ffi::{OsStr, c_void},
    fs::File,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        GENERIC_READ, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    Security::{SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES},
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_JOB_MEMORY, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
            JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
            TerminateJobObject,
        },
        Pipes::CreatePipe,
        SystemInformation::GetWindowsDirectoryW,
        Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
            WaitForSingleObject,
        },
    },
};

use super::{NativeError, wide};

const MAX_PROCESS_COUNT: u32 = 256;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024;
const MIN_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const TERMINATION_EXIT_CODE: u32 = 0xffff_fffe;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const JOB_ACCOUNTING_SETTLE_TIME: Duration = Duration::from_millis(100);
const CHILD_PROCESS_OVERRIDE: u32 = 0x0000_0002;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandLimits {
    pub memory_bytes: u64,
    pub process_count: u32,
}

pub struct CommandLaunch<'a> {
    pub executable: &'a Path,
    pub arguments: &'a [String],
    pub candidate: &'a Path,
    pub working_directory: &'a Path,
    pub runtime: &'a Path,
    pub image: &'a Path,
    pub limits: CommandLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandCompletion {
    Clean { exit_code: u32 },
    DescendantsTerminated,
}

pub struct CommandProcess {
    process: OwnedHandle,
    job: OwnedHandle,
    stdout: Option<File>,
    stderr: Option<File>,
    stopped: bool,
}

impl CommandProcess {
    pub fn take_stdout(&mut self) -> Option<File> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<File> {
        self.stderr.take()
    }

    pub fn wait_root(&self, timeout: Duration) -> Result<Option<u32>, NativeError> {
        let milliseconds = bounded_milliseconds(timeout);
        // SAFETY: The owned process handle remains alive throughout this wait.
        let result = unsafe { WaitForSingleObject(raw(&self.process), milliseconds) };
        if result == WAIT_TIMEOUT {
            return Ok(None);
        }
        if result != WAIT_OBJECT_0 {
            return Err(NativeError::last("process-wait"));
        }
        let mut exit_code = 0u32;
        // SAFETY: The process handle is valid and `exit_code` is a writable out-parameter.
        if unsafe { GetExitCodeProcess(raw(&self.process), &mut exit_code) } == 0 {
            return Err(NativeError::last("process-exit"));
        }
        Ok(Some(exit_code))
    }

    pub fn complete_and_verify(
        mut self,
        timeout: Duration,
    ) -> Result<CommandCompletion, NativeError> {
        let deadline = Instant::now() + timeout;
        let exit_code = self
            .wait_root(deadline.saturating_duration_since(Instant::now()))?
            .ok_or_else(|| NativeError::code("process-active", 0))?;
        let settle_deadline = (Instant::now() + JOB_ACCOUNTING_SETTLE_TIME).min(deadline);
        loop {
            if active_processes(&self.job)? == 0 {
                self.stopped = true;
                return Ok(CommandCompletion::Clean { exit_code });
            }
            if Instant::now() >= settle_deadline {
                break;
            }
            thread::sleep(POLL_INTERVAL);
        }
        self.stop(deadline.saturating_duration_since(Instant::now()))?;
        Ok(CommandCompletion::DescendantsTerminated)
    }

    pub fn terminate_and_verify(mut self, timeout: Duration) -> Result<(), NativeError> {
        self.stop(timeout)
    }

    fn stop(&mut self, timeout: Duration) -> Result<(), NativeError> {
        if self.stopped {
            return Ok(());
        }
        // SAFETY: `job` owns the configured operation Job handle.
        if unsafe { TerminateJobObject(raw(&self.job), TERMINATION_EXIT_CODE) } == 0 {
            return Err(NativeError::last("job-terminate"));
        }
        let deadline = Instant::now() + timeout;
        let root_wait = deadline.saturating_duration_since(Instant::now());
        if self.wait_root(root_wait)?.is_none() {
            return Err(NativeError::code("process-active", 0));
        }
        loop {
            if active_processes(&self.job)? == 0 {
                self.stopped = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(NativeError::code("job-active", 0));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }
}

impl Drop for CommandProcess {
    fn drop(&mut self) {
        if !self.stopped {
            // SAFETY: Failure still leaves kill-on-close enforcement when the owned Job handle drops.
            unsafe {
                let _ = TerminateJobObject(raw(&self.job), TERMINATION_EXIT_CODE);
            }
        }
    }
}

pub(super) fn launch(
    sid: *mut c_void,
    request: CommandLaunch<'_>,
) -> Result<CommandProcess, NativeError> {
    validate(&request)?;
    if sid.is_null() {
        return Err(NativeError::code("launch-sid", 0));
    }
    let executable = launch_path(request.executable)?;
    let working_directory = launch_path(request.working_directory)?;
    let command_line = command_line(&executable, request.arguments)?;
    let environment = environment_block(request.runtime, request.image)?;
    let io = ChildIo::new()?;
    let job = create_job(request.limits)?;
    let mut attributes = AttributeList::new(3)?;
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: std::ptr::null_mut(),
        CapabilityCount: 0,
        Reserved: 0,
    };
    attributes.update(
        PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
        (&raw const capabilities).cast(),
        std::mem::size_of::<SECURITY_CAPABILITIES>(),
        "attribute-capabilities",
    )?;
    let child_process_policy = CHILD_PROCESS_OVERRIDE;
    attributes.update(
        PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize,
        (&raw const child_process_policy).cast(),
        std::mem::size_of::<u32>(),
        "attribute-child-process",
    )?;
    let inherited = io.raw_handles();
    attributes.update(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        inherited.as_ptr().cast(),
        std::mem::size_of_val(inherited.as_slice()),
        "attribute-handles",
    )?;

    let executable = wide(executable.as_os_str())?;
    let working_directory = wide(working_directory.as_os_str())?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>())
        .map_err(|_| NativeError::code("startup-size", 0))?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = raw(&io.input);
    startup.StartupInfo.hStdOutput = raw(&io.stdout_child);
    startup.StartupInfo.hStdError = raw(&io.stderr_child);
    startup.lpAttributeList = attributes.pointer();
    let mut information = PROCESS_INFORMATION::default();
    let mut command_line = command_line;
    let flags = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_SUSPENDED
        | CREATE_BREAKAWAY_FROM_JOB
        | CREATE_NO_WINDOW
        | CREATE_UNICODE_ENVIRONMENT;
    // SAFETY: All buffers remain live, the command line is mutable, and only the exact three listed handles are inheritable.
    let created = unsafe {
        CreateProcessW(
            executable.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            flags,
            environment.as_ptr().cast(),
            working_directory.as_ptr(),
            &raw const startup.StartupInfo,
            &mut information,
        )
    };
    if created == 0 {
        return Err(NativeError::last("process-create"));
    }
    let suspended = SuspendedProcess::new(information);
    // SAFETY: Both handles are valid and the process has not executed because its initial thread remains suspended.
    if unsafe { AssignProcessToJobObject(raw(&job), raw(suspended.process())) } == 0 {
        return Err(NativeError::last("job-assign"));
    }
    // SAFETY: The owned initial-thread handle is valid and has not previously been resumed by Morons.
    if unsafe { ResumeThread(raw(&suspended.thread)) } == u32::MAX {
        return Err(NativeError::last("process-resume"));
    }
    let process = suspended.into_process();
    let (stdout, stderr) = io.into_parent_streams();
    Ok(CommandProcess {
        process,
        job,
        stdout: Some(stdout),
        stderr: Some(stderr),
        stopped: false,
    })
}

fn validate(request: &CommandLaunch<'_>) -> Result<(), NativeError> {
    if request.limits.memory_bytes < MIN_MEMORY_BYTES
        || request.limits.memory_bytes > MAX_MEMORY_BYTES
        || request.limits.process_count == 0
        || request.limits.process_count > MAX_PROCESS_COUNT
        || request.arguments.len() > MAX_ARGUMENTS
    {
        return Err(NativeError::code("limits", 0));
    }
    for path in [
        request.executable,
        request.candidate,
        request.working_directory,
        request.runtime,
        request.image,
    ] {
        if !path.is_absolute() || path.to_str().is_none() {
            return Err(NativeError::code("launch-path", 0));
        }
    }
    if !request.executable.is_file()
        || !request.candidate.is_dir()
        || !request.working_directory.is_dir()
        || !request.runtime.is_dir()
        || !request.image.is_dir()
    {
        return Err(NativeError::code("launch-node", 0));
    }
    let argument_bytes = request.arguments.iter().try_fold(0usize, |total, value| {
        if value.len() > MAX_ARGUMENT_BYTES || value.contains('\0') {
            return None;
        }
        total.checked_add(value.len())
    });
    if argument_bytes.is_none_or(|total| total > MAX_ARGUMENT_TOTAL_BYTES) {
        return Err(NativeError::code("command-arguments", 0));
    }
    let candidate = request
        .candidate
        .canonicalize()
        .map_err(|_| NativeError::last("candidate-canonical"))?;
    let runtime = request
        .runtime
        .canonicalize()
        .map_err(|_| NativeError::last("runtime-canonical"))?;
    let image = request
        .image
        .canonicalize()
        .map_err(|_| NativeError::last("image-canonical"))?;
    let executable = request
        .executable
        .canonicalize()
        .map_err(|_| NativeError::last("executable-canonical"))?;
    let working_directory = request
        .working_directory
        .canonicalize()
        .map_err(|_| NativeError::last("working-directory-canonical"))?;
    if !executable.starts_with(&image)
        || !working_directory.starts_with(&candidate)
        || overlaps(&candidate, &runtime)
        || overlaps(&candidate, &image)
        || overlaps(&runtime, &image)
    {
        return Err(NativeError::code("command-scope", 0));
    }
    Ok(())
}

fn create_job(limits: CommandLimits) -> Result<OwnedHandle, NativeError> {
    // SAFETY: no security attributes or global name are supplied.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    let job = owned(handle, "job-create")?;
    let memory =
        usize::try_from(limits.memory_bytes).map_err(|_| NativeError::code("job-memory", 0))?;
    let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
    information.BasicLimitInformation.ActiveProcessLimit = limits.process_count;
    information.ProcessMemoryLimit = memory;
    information.JobMemoryLimit = memory;
    // SAFETY: The Job handle is valid and `information` has the exact required structure and size.
    if unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            (&raw const information).cast(),
            u32::try_from(std::mem::size_of_val(&information))
                .map_err(|_| NativeError::code("job-size", 0))?,
        )
    } == 0
    {
        return Err(NativeError::last("job-configure"));
    }
    let mut verified = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: The Job handle is valid and `verified` is a writable exact-size output buffer.
    if unsafe {
        QueryInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            (&raw mut verified).cast(),
            u32::try_from(std::mem::size_of_val(&verified))
                .map_err(|_| NativeError::code("job-size", 0))?,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(NativeError::last("job-verify"));
    }
    if verified.BasicLimitInformation.LimitFlags != information.BasicLimitInformation.LimitFlags
        || verified.BasicLimitInformation.ActiveProcessLimit != limits.process_count
        || verified.ProcessMemoryLimit != memory
        || verified.JobMemoryLimit != memory
    {
        return Err(NativeError::code("job-policy", 0));
    }
    Ok(job)
}

fn active_processes(job: &OwnedHandle) -> Result<u32, NativeError> {
    let mut information = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: The Job handle is valid and `information` is a writable exact-size output buffer.
    if unsafe {
        QueryInformationJobObject(
            raw(job),
            JobObjectBasicAccountingInformation,
            (&raw mut information).cast(),
            u32::try_from(std::mem::size_of_val(&information))
                .map_err(|_| NativeError::code("job-size", 0))?,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(NativeError::last("job-accounting"));
    }
    Ok(information.ActiveProcesses)
}

struct SuspendedProcess {
    process: Option<OwnedHandle>,
    thread: OwnedHandle,
}

impl SuspendedProcess {
    fn new(information: PROCESS_INFORMATION) -> Self {
        // SAFETY: Successful process creation returned two distinct handles transferred exactly once.
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess.cast()) };
        // SAFETY: as above, this is the distinct initial-thread handle.
        let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread.cast()) };
        Self {
            process: Some(process),
            thread,
        }
    }

    fn process(&self) -> &OwnedHandle {
        self.process
            .as_ref()
            .expect("suspended process is present before transfer")
    }

    fn into_process(mut self) -> OwnedHandle {
        self.process
            .take()
            .expect("suspended process is transferred exactly once")
    }
}

impl Drop for SuspendedProcess {
    fn drop(&mut self) {
        if let Some(process) = &self.process {
            // SAFETY: The still-owned suspended process handle is terminated before any unassigned process can execute.
            unsafe {
                let _ = TerminateProcess(raw(process), TERMINATION_EXIT_CODE);
            }
        }
    }
}

struct ChildIo {
    input: OwnedHandle,
    stdout_child: OwnedHandle,
    stderr_child: OwnedHandle,
    stdout_parent: Option<OwnedHandle>,
    stderr_parent: Option<OwnedHandle>,
}

impl ChildIo {
    fn new() -> Result<Self, NativeError> {
        let (stdout_parent, stdout_child) = output_pipe()?;
        let (stderr_parent, stderr_child) = output_pipe()?;
        Ok(Self {
            input: open_null(GENERIC_READ)?,
            stdout_child,
            stderr_child,
            stdout_parent: Some(stdout_parent),
            stderr_parent: Some(stderr_parent),
        })
    }

    fn raw_handles(&self) -> [HANDLE; 3] {
        [
            raw(&self.input),
            raw(&self.stdout_child),
            raw(&self.stderr_child),
        ]
    }

    fn into_parent_streams(mut self) -> (File, File) {
        let stdout = self
            .stdout_parent
            .take()
            .expect("stdout parent endpoint is transferred exactly once");
        let stderr = self
            .stderr_parent
            .take()
            .expect("stderr parent endpoint is transferred exactly once");
        (File::from(stdout), File::from(stderr))
    }
}

fn output_pipe() -> Result<(OwnedHandle, OwnedHandle), NativeError> {
    let attributes = inheritable_attributes()?;
    let mut parent = std::ptr::null_mut();
    let mut child = std::ptr::null_mut();
    // SAFETY: Both output pointers and the inheritable security attributes are valid for the call.
    if unsafe { CreatePipe(&mut parent, &mut child, &attributes, 0) } == 0 {
        return Err(NativeError::last("pipe-create"));
    }
    let parent = owned(parent, "pipe-parent")?;
    let child = owned(child, "pipe-child")?;
    // SAFETY: The owned parent endpoint is valid and only its inheritance flag is cleared.
    if unsafe { SetHandleInformation(raw(&parent), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(NativeError::last("pipe-inheritance"));
    }
    Ok((parent, child))
}

fn open_null(access: u32) -> Result<OwnedHandle, NativeError> {
    let name = wide(OsStr::new("NUL"))?;
    let attributes = inheritable_attributes()?;
    // SAFETY: The name and attributes are valid and the returned handle is transferred exactly once.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    owned(handle, "null-open")
}

fn inheritable_attributes() -> Result<SECURITY_ATTRIBUTES, NativeError> {
    Ok(SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| NativeError::code("handle-size", 0))?,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    })
}

struct AttributeList {
    storage: Vec<usize>,
    initialized: bool,
}

impl AttributeList {
    fn new(count: u32) -> Result<Self, NativeError> {
        let mut bytes = 0usize;
        // SAFETY: This documented size query uses a null list and writes only the required byte count.
        unsafe {
            let _ = InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(NativeError::last("attribute-size"));
        }
        let words = bytes
            .checked_add(std::mem::size_of::<usize>() - 1)
            .ok_or_else(|| NativeError::code("attribute-size", 0))?
            / std::mem::size_of::<usize>();
        let mut list = Self {
            storage: vec![0usize; words],
            initialized: false,
        };
        // SAFETY: Aligned storage has the queried size and remains fixed for the list lifetime.
        if unsafe { InitializeProcThreadAttributeList(list.pointer(), count, 0, &mut bytes) } == 0 {
            return Err(NativeError::last("attribute-init"));
        }
        list.initialized = true;
        Ok(list)
    }

    fn pointer(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }

    fn update(
        &mut self,
        attribute: usize,
        value: *const c_void,
        size: usize,
        stage: &'static str,
    ) -> Result<(), NativeError> {
        if value.is_null() || size == 0 {
            return Err(NativeError::code(stage, 0));
        }
        // SAFETY: The list is initialized and each caller-owned value outlives process creation.
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                attribute,
                value,
                size,
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        } == 0
        {
            return Err(NativeError::last(stage));
        }
        Ok(())
    }
}

impl Drop for AttributeList {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: The initialized list is deleted exactly once before its stable storage is released.
            unsafe {
                DeleteProcThreadAttributeList(self.pointer());
            }
        }
    }
}

fn environment_block(runtime: &Path, image: &Path) -> Result<Vec<u16>, NativeError> {
    let windows = windows_directory()?;
    let windows = windows
        .to_str()
        .ok_or_else(|| NativeError::code("environment", 0))?;
    let system_drive = windows
        .get(..2)
        .filter(|value| value.as_bytes().get(1) == Some(&b':'))
        .ok_or_else(|| NativeError::code("system-drive", 0))?;
    let temporary = runtime.join("tmp");
    let home = runtime.join("home");
    let local = runtime.join("local-app-data");
    let roaming = runtime.join("app-data");
    let public = runtime.join("public");
    let cargo = runtime.join("cargo-home");
    for directory in [&temporary, &home, &local, &roaming, &public, &cargo] {
        if !directory.is_dir() {
            return Err(NativeError::code("runtime-directory", 0));
        }
    }
    let path = environment_path(&image.join("bin"))?;
    let temporary = environment_path(&temporary)?;
    let home = environment_path(&home)?;
    let local = environment_path(&local)?;
    let roaming = environment_path(&roaming)?;
    let public = environment_path(&public)?;
    let cargo = environment_path(&cargo)?;
    let processor = if cfg!(target_arch = "aarch64") {
        "ARM64"
    } else {
        "AMD64"
    };
    let mut entries = vec![
        ("ALLUSERSPROFILE", public.clone()),
        ("APPDATA", roaming),
        ("CARGO_HOME", cargo),
        ("CARGO_NET_OFFLINE", "true".to_owned()),
        ("COMPUTERNAME", "MORONS-SANDBOX".to_owned()),
        ("ComSpec", format!(r"{windows}\System32\cmd.exe")),
        ("HOME", home.clone()),
        ("LOCALAPPDATA", local),
        ("MORONS_SANDBOX", "1".to_owned()),
        ("NUMBER_OF_PROCESSORS", "1".to_owned()),
        ("OS", "Windows_NT".to_owned()),
        ("PATH", path),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD".to_owned()),
        ("PROCESSOR_ARCHITECTURE", processor.to_owned()),
        ("PUBLIC", public),
        ("SYSTEMROOT", windows.to_owned()),
        ("SystemDrive", system_drive.to_owned()),
        ("TERM", "dumb".to_owned()),
        ("NO_COLOR", "1".to_owned()),
        ("TEMP", temporary.clone()),
        ("TMP", temporary),
        ("USERNAME", "morons-sandbox".to_owned()),
        ("USERPROFILE", home),
        ("WINDIR", windows.to_owned()),
    ];
    entries.sort_by_key(|(name, _)| name.to_ascii_lowercase());
    let mut block = Vec::new();
    for (name, value) in entries {
        if value.contains(['\0', '\r', '\n']) {
            return Err(NativeError::code("environment", 0));
        }
        block.extend(format!("{name}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn environment_path(path: &Path) -> Result<String, NativeError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| NativeError::code("environment", 0))
}

fn windows_directory() -> Result<PathBuf, NativeError> {
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: `buffer` is writable for the supplied length.
    let length = unsafe {
        GetWindowsDirectoryW(
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).map_err(|_| NativeError::code("windows-path", 0))?,
        )
    };
    let length = usize::try_from(length).map_err(|_| NativeError::code("windows-path", 0))?;
    if length == 0 || length >= buffer.len() {
        return Err(NativeError::last("windows-path"));
    }
    buffer.truncate(length);
    Ok(PathBuf::from(
        String::from_utf16(&buffer).map_err(|_| NativeError::code("windows-path", 0))?,
    ))
}

fn command_line(executable: &Path, arguments: &[String]) -> Result<Vec<u16>, NativeError> {
    let executable = executable
        .to_str()
        .ok_or_else(|| NativeError::code("command-line", 0))?;
    let mut command = String::new();
    append_quoted_argument(&mut command, executable)?;
    for argument in arguments {
        command.push(' ');
        append_quoted_argument(&mut command, argument)?;
    }
    Ok(command.encode_utf16().chain(std::iter::once(0)).collect())
}

fn append_quoted_argument(command: &mut String, value: &str) -> Result<(), NativeError> {
    if value.contains('\0') {
        return Err(NativeError::code("command-line", 0));
    }
    command.push('"');
    let mut backslashes = 0usize;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            let escaped = backslashes
                .checked_mul(2)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| NativeError::code("command-line", 0))?;
            command.extend(std::iter::repeat_n('\\', escaped));
            command.push('"');
        } else {
            command.extend(std::iter::repeat_n('\\', backslashes));
            command.push(character);
        }
        backslashes = 0;
    }
    let escaped = backslashes
        .checked_mul(2)
        .ok_or_else(|| NativeError::code("command-line", 0))?;
    command.extend(std::iter::repeat_n('\\', escaped));
    command.push('"');
    Ok(())
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn launch_path(path: &Path) -> Result<PathBuf, NativeError> {
    let value = path
        .to_str()
        .ok_or_else(|| NativeError::code("launch-path", 0))?;
    Ok(value
        .strip_prefix(r"\\?\")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf()))
}

fn owned(handle: HANDLE, stage: &'static str) -> Result<OwnedHandle, NativeError> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(NativeError::last(stage));
    }
    // SAFETY: The successful Windows call transferred one handle that is wrapped exactly once.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.cast()) })
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle().cast()
}

fn bounded_milliseconds(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX - 1)
}
