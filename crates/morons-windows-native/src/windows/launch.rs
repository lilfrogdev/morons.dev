use std::{
    ffi::{OsStr, c_void},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
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
        SystemInformation::GetWindowsDirectoryW,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
            WaitForSingleObject,
        },
    },
};

use super::{NativeError, wide};

const MAX_PROCESS_COUNT: u32 = 256;
const MIN_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_MEMORY_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const TERMINATION_EXIT_CODE: u32 = 0xffff_fffe;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootstrapLimits {
    pub memory_bytes: u64,
    pub process_count: u32,
}

pub struct BootstrapLaunch<'a> {
    pub executable: &'a Path,
    pub working_directory: &'a Path,
    pub runtime: &'a Path,
    pub input: &'a Path,
    pub output: &'a Path,
    pub gate: &'a Path,
    pub done: &'a Path,
    pub limits: BootstrapLimits,
}

pub struct BootstrapProcess {
    process: OwnedHandle,
    job: OwnedHandle,
    process_id: u32,
    stopped: bool,
}

impl BootstrapProcess {
    pub fn id(&self) -> u32 {
        self.process_id
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

impl Drop for BootstrapProcess {
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
    request: BootstrapLaunch<'_>,
) -> Result<BootstrapProcess, NativeError> {
    validate(&request)?;
    if sid.is_null() {
        return Err(NativeError::code("launch-sid", 0));
    }
    let executable = launch_path(request.executable)?;
    let working_directory = launch_path(request.working_directory)?;
    let command_line = command_line(&executable, &request)?;
    let environment = environment_block(request.runtime)?;
    let null_handles = NullHandles::new()?;
    let job = create_job(request.limits)?;
    let mut attributes = AttributeList::new(2)?;
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
    let inherited = null_handles.raw_handles();
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
    startup.StartupInfo.hStdInput = raw(&null_handles.input);
    startup.StartupInfo.hStdOutput = raw(&null_handles.output);
    startup.StartupInfo.hStdError = raw(&null_handles.error);
    startup.lpAttributeList = attributes.pointer();
    let mut information = PROCESS_INFORMATION::default();
    let mut command_line = command_line;
    let flags = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_SUSPENDED
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
    let process_id = suspended.process_id;
    let process = suspended.into_process();
    Ok(BootstrapProcess {
        process,
        job,
        process_id,
        stopped: false,
    })
}

fn validate(request: &BootstrapLaunch<'_>) -> Result<(), NativeError> {
    if request.limits.memory_bytes < MIN_MEMORY_BYTES
        || request.limits.memory_bytes > MAX_MEMORY_BYTES
        || request.limits.process_count == 0
        || request.limits.process_count > MAX_PROCESS_COUNT
    {
        return Err(NativeError::code("limits", 0));
    }
    for path in [
        request.executable,
        request.working_directory,
        request.runtime,
        request.input,
        request.output,
        request.gate,
        request.done,
    ] {
        if !path.is_absolute() || path.to_str().is_none() {
            return Err(NativeError::code("launch-path", 0));
        }
    }
    if !request.executable.is_file()
        || !request.working_directory.is_dir()
        || !request.runtime.is_dir()
    {
        return Err(NativeError::code("launch-node", 0));
    }
    let runtime = request
        .runtime
        .canonicalize()
        .map_err(|_| NativeError::last("runtime-canonical"))?;
    for path in [request.input, request.output, request.gate, request.done] {
        let parent = path
            .parent()
            .ok_or_else(|| NativeError::code("control-parent", 0))?
            .canonicalize()
            .map_err(|_| NativeError::last("control-canonical"))?;
        if !parent.starts_with(&runtime) {
            return Err(NativeError::code("control-scope", 0));
        }
    }
    Ok(())
}

fn create_job(limits: BootstrapLimits) -> Result<OwnedHandle, NativeError> {
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
    process_id: u32,
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
            process_id: information.dwProcessId,
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

struct NullHandles {
    input: OwnedHandle,
    output: OwnedHandle,
    error: OwnedHandle,
}

impl NullHandles {
    fn new() -> Result<Self, NativeError> {
        Ok(Self {
            input: open_null(GENERIC_READ)?,
            output: open_null(GENERIC_WRITE)?,
            error: open_null(GENERIC_WRITE)?,
        })
    }

    fn raw_handles(&self) -> [HANDLE; 3] {
        [raw(&self.input), raw(&self.output), raw(&self.error)]
    }
}

fn open_null(access: u32) -> Result<OwnedHandle, NativeError> {
    let name = wide(OsStr::new("NUL"))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| NativeError::code("handle-size", 0))?,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
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

fn environment_block(runtime: &Path) -> Result<Vec<u16>, NativeError> {
    let windows = windows_directory()?;
    let temporary = runtime.join("tmp");
    if !temporary.is_dir() {
        return Err(NativeError::code("runtime-temp", 0));
    }
    let mut entries = [
        ("SystemRoot", windows.as_os_str()),
        ("TEMP", temporary.as_os_str()),
        ("TMP", temporary.as_os_str()),
        ("WINDIR", windows.as_os_str()),
    ];
    entries.sort_by_key(|(name, _)| name.to_ascii_lowercase());
    let mut block = Vec::new();
    for (name, value) in entries {
        let value = value
            .to_str()
            .ok_or_else(|| NativeError::code("environment", 0))?;
        if value.contains(['\0', '\r', '\n']) {
            return Err(NativeError::code("environment", 0));
        }
        block.extend(format!("{name}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    Ok(block)
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

fn command_line(executable: &Path, request: &BootstrapLaunch<'_>) -> Result<Vec<u16>, NativeError> {
    let values = [
        executable,
        Path::new("--windows-command-stage"),
        request.input,
        request.output,
        request.gate,
        request.done,
    ];
    let mut command = String::new();
    for (index, value) in values.into_iter().enumerate() {
        let value = value
            .to_str()
            .ok_or_else(|| NativeError::code("command-line", 0))?;
        if value.contains(['\0', '"', '\r', '\n']) {
            return Err(NativeError::code("command-line", 0));
        }
        if index != 0 {
            command.push(' ');
        }
        command.push('"');
        command.push_str(value);
        command.push('"');
    }
    Ok(command.encode_utf16().chain(std::iter::once(0)).collect())
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
