use std::{
    env,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::{
    fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
    process::CommandExt as _,
};

use sha2::{Digest as _, Sha256};

use super::super::{
    ToolErrorKind,
    bash::{PlatformJob, terminate_tree},
};
use crate::provider::ProviderCancellation;

const UV_VERSION: &str = "0.12.9";
const PYTHON_VERSION: &str = "3.11.15";
const JUPYTER_CLIENT_VERSION: &str = "8.6.3";
const IPYKERNEL_VERSION: &str = "6.30.1";
const RUNTIME_DIRECTORY: &str = "runtime-v1";
const STAGING_DIRECTORY: &str = ".runtime-v1.staging";
const LOCK_FILE: &str = ".bootstrap.lock";
const MANIFEST_FILE: &str = "MANIFEST.txt";
const REQUIREMENTS_FILE: &str = "requirements.txt";
const REQUIREMENTS_SHA256: &str =
    "aef4426a712442082dd3762ce08ca17e4db3f117a7cb8a232e979700de2217ac";
#[cfg(test)]
const REQUIREMENTS_INPUT: &str = include_str!("../../../runtime/ipython-requirements.in");
const REQUIREMENTS: &str = include_str!("../../../runtime/ipython-requirements.txt");
const UV_ASSETS: &str = include_str!("../../../runtime/uv-assets.txt");
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_UV_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MANAGED_RUNTIME_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MANAGED_RUNTIME_ENTRIES: usize = 100_000;

pub(super) enum PythonRuntime {
    Override(OsString),
    Managed(ManagedPythonRuntime),
}

impl PythonRuntime {
    pub(super) fn configured(managed_root: PathBuf) -> Self {
        env::var_os("MORONS_PYTHON")
            .filter(|value| !value.is_empty())
            .map_or_else(
                || Self::Managed(ManagedPythonRuntime::new(managed_root)),
                Self::Override,
            )
    }

    pub(super) fn resolve(
        &self,
        cancellation: &ProviderCancellation,
    ) -> Result<ResolvedPython, ToolErrorKind> {
        match self {
            Self::Override(executable) => Ok(ResolvedPython {
                executable: executable.clone(),
                isolated: false,
            }),
            Self::Managed(runtime) => {
                runtime
                    .ensure(cancellation)
                    .map(|executable| ResolvedPython {
                        executable,
                        isolated: true,
                    })
            }
        }
    }

    #[cfg(test)]
    pub(super) fn test_override() -> Self {
        Self::Override(configured_test_python())
    }

    #[cfg(test)]
    pub(super) fn managed_for_test(root: PathBuf, uv: PathBuf) -> Self {
        Self::Managed(ManagedPythonRuntime::with_uv(root, uv))
    }
}

pub(super) struct ResolvedPython {
    executable: OsString,
    isolated: bool,
}

impl ResolvedPython {
    pub(super) fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        if self.isolated {
            configure_managed_environment(&mut command);
        }
        command
    }
}

pub(super) struct ManagedPythonRuntime {
    root: PathBuf,
    packaged_uv: Option<PathBuf>,
    cached: Mutex<Option<OsString>>,
}

impl ManagedPythonRuntime {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            packaged_uv: None,
            cached: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_uv(root: PathBuf, uv: PathBuf) -> Self {
        Self {
            root,
            packaged_uv: Some(uv),
            cached: Mutex::new(None),
        }
    }

    fn ensure(&self, cancellation: &ProviderCancellation) -> Result<OsString, ToolErrorKind> {
        if cancellation.is_cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        if let Some(cached) = self
            .cached
            .lock()
            .map_err(|_| ToolErrorKind::KernelUnavailable)?
            .clone()
        {
            return Ok(cached);
        }
        let deadline = Instant::now() + BOOTSTRAP_TIMEOUT;
        ensure_private_directory(&self.root)?;
        let lock = open_private_lock(&self.root.join(LOCK_FILE))?;
        acquire_lock(&lock, cancellation, deadline)?;
        if !managed_tree_is_bounded(&self.root)? {
            return Err(ToolErrorKind::ResourceLimit);
        }
        let runtime = self.root.join(RUNTIME_DIRECTORY);
        if self.runtime_is_valid(&runtime, cancellation, deadline) {
            return self.cache(runtime_python(&runtime));
        }

        let uv = self
            .packaged_uv
            .clone()
            .map_or_else(discover_packaged_uv, Ok)?;
        if self.packaged_uv.is_none() {
            validate_packaged_uv(&uv)?;
        }
        let staging = self.root.join(STAGING_DIRECTORY);
        remove_managed_path(&staging)?;
        ensure_private_directory(&staging)?;
        let cache = self.root.join("cache");
        let managed_python = self.root.join("managed");
        ensure_private_directory(&cache)?;
        ensure_private_directory(&managed_python)?;

        self.run_uv(
            &uv,
            [
                OsString::from("--no-config"),
                OsString::from("--color"),
                OsString::from("never"),
                OsString::from("--no-progress"),
                OsString::from("--cache-dir"),
                cache.as_os_str().to_owned(),
                OsString::from("python"),
                OsString::from("install"),
                OsString::from("--managed-python"),
                OsString::from("--no-bin"),
                OsString::from("--install-dir"),
                managed_python.as_os_str().to_owned(),
                OsString::from(managed_python_request()),
            ],
            cancellation,
            deadline,
        )?;
        let base_python = find_managed_python(&managed_python)?;
        self.run_uv(
            &uv,
            [
                OsString::from("--no-config"),
                OsString::from("--color"),
                OsString::from("never"),
                OsString::from("--no-progress"),
                OsString::from("--cache-dir"),
                cache.as_os_str().to_owned(),
                OsString::from("venv"),
                OsString::from("--no-project"),
                OsString::from("--relocatable"),
                OsString::from("--managed-python"),
                OsString::from("--no-python-downloads"),
                OsString::from("--python"),
                base_python.as_os_str().to_owned(),
                staging.as_os_str().to_owned(),
            ],
            cancellation,
            deadline,
        )?;
        write_private_file(&staging.join(REQUIREMENTS_FILE), REQUIREMENTS.as_bytes())?;
        let staging_python = runtime_python(&staging);
        if !path_is_inside(&staging_python, &self.root) {
            remove_managed_path(&staging)?;
            return Err(ToolErrorKind::KernelUnavailable);
        }
        self.run_uv(
            &uv,
            [
                OsString::from("--no-config"),
                OsString::from("--color"),
                OsString::from("never"),
                OsString::from("--no-progress"),
                OsString::from("--cache-dir"),
                cache.as_os_str().to_owned(),
                OsString::from("pip"),
                OsString::from("install"),
                OsString::from("--python"),
                staging_python.as_os_str().to_owned(),
                OsString::from("--require-hashes"),
                OsString::from("--only-binary"),
                OsString::from(":all:"),
                OsString::from("--index-url"),
                OsString::from("https://pypi.org/simple"),
                OsString::from("--no-python-downloads"),
                OsString::from("-r"),
                staging.join(REQUIREMENTS_FILE).into_os_string(),
            ],
            cancellation,
            deadline,
        )?;
        if !validate_python(&staging_python, cancellation, deadline) {
            remove_managed_path(&staging)?;
            return Err(ToolErrorKind::KernelUnavailable);
        }
        write_private_file(&staging.join(MANIFEST_FILE), runtime_manifest().as_bytes())?;
        sync_directory(&staging)?;
        remove_managed_path(&runtime)?;
        fs::rename(&staging, &runtime).map_err(|_| ToolErrorKind::KernelUnavailable)?;
        sync_directory(&self.root)?;
        self.cache(runtime_python(&runtime))
    }

    fn runtime_is_valid(
        &self,
        runtime: &Path,
        cancellation: &ProviderCancellation,
        deadline: Instant,
    ) -> bool {
        if validate_private_directory(runtime).is_err()
            || fs::read(runtime.join(MANIFEST_FILE)).ok().as_deref()
                != Some(runtime_manifest().as_bytes())
        {
            return false;
        }
        let python = runtime_python(runtime);
        path_is_inside(&python, &self.root) && validate_python(&python, cancellation, deadline)
    }

    fn run_uv<const N: usize>(
        &self,
        uv: &Path,
        arguments: [OsString; N],
        cancellation: &ProviderCancellation,
        deadline: Instant,
    ) -> Result<(), ToolErrorKind> {
        let mut command = Command::new(uv);
        command.args(arguments).current_dir(&self.root);
        run_managed_process(command, cancellation, deadline)?;
        if managed_tree_is_bounded(&self.root)? {
            Ok(())
        } else {
            Err(ToolErrorKind::ResourceLimit)
        }
    }

    fn cache(&self, executable: PathBuf) -> Result<OsString, ToolErrorKind> {
        let executable = executable.into_os_string();
        *self
            .cached
            .lock()
            .map_err(|_| ToolErrorKind::KernelUnavailable)? = Some(executable.clone());
        Ok(executable)
    }
}

fn runtime_manifest() -> String {
    format!(
        "format=1\nuv={UV_VERSION}\npython={PYTHON_VERSION}\npython_target={}\njupyter_client={JUPYTER_CLIENT_VERSION}\nipykernel={IPYKERNEL_VERSION}\nrequirements_sha256={REQUIREMENTS_SHA256}\n",
        managed_python_target()
    )
}

fn managed_python_request() -> &'static str {
    if cfg!(all(windows, target_arch = "aarch64")) {
        "cpython-3.11.15-windows-x86_64-none"
    } else {
        PYTHON_VERSION
    }
}

fn managed_python_target() -> &'static str {
    match (env::consts::OS, env::consts::ARCH) {
        ("windows", "aarch64") => "x86_64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        _ => "unsupported",
    }
}

fn validate_python(python: &Path, cancellation: &ProviderCancellation, deadline: Instant) -> bool {
    let mut command = Command::new(python);
    command.args([
        "-I",
        "-c",
        "import ipykernel,jupyter_client; assert ipykernel.__version__ == '6.30.1'; assert jupyter_client.__version__ == '8.6.3'",
    ]);
    run_managed_process(command, cancellation, deadline).is_ok()
}

fn runtime_python(runtime: &Path) -> PathBuf {
    #[cfg(windows)]
    return runtime.join("Scripts").join("python.exe");
    #[cfg(not(windows))]
    runtime.join("bin").join("python")
}

fn find_managed_python(root: &Path) -> Result<PathBuf, ToolErrorKind> {
    let prefix = format!("cpython-{PYTHON_VERSION}-");
    let mut candidates = fs::read_dir(root)
        .map_err(|_| ToolErrorKind::KernelUnavailable)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .filter_map(|entry| {
            let path = entry.path();
            validate_managed_child_directory(&path).ok()?;
            let python = managed_python_executable(&path);
            path_is_inside(&python, root).then_some(python)
        });
    let python = candidates.next().ok_or(ToolErrorKind::KernelUnavailable)?;
    if candidates.next().is_some() {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(python)
}

fn managed_python_executable(root: &Path) -> PathBuf {
    #[cfg(windows)]
    return root.join("python.exe");
    #[cfg(not(windows))]
    root.join("bin").join("python3.11")
}

fn discover_packaged_uv() -> Result<PathBuf, ToolErrorKind> {
    let current = env::current_exe().map_err(|_| ToolErrorKind::KernelUnavailable)?;
    let current = fs::canonicalize(current).map_err(|_| ToolErrorKind::KernelUnavailable)?;
    let expected_server = if cfg!(windows) {
        "morons-server.exe"
    } else {
        "morons-server"
    };
    if current.file_name().and_then(OsStr::to_str) != Some(expected_server) {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    let parent = current.parent().ok_or(ToolErrorKind::KernelUnavailable)?;
    validate_installation_directory(parent)?;
    let uv = parent.join(if cfg!(windows) {
        "morons-uv.exe"
    } else {
        "morons-uv"
    });
    let metadata = fs::symlink_metadata(&uv).map_err(|_| ToolErrorKind::KernelUnavailable)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    let canonical = fs::canonicalize(&uv).map_err(|_| ToolErrorKind::KernelUnavailable)?;
    if canonical.parent() != Some(parent) {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(canonical)
}

fn validate_installation_directory(path: &Path) -> Result<(), ToolErrorKind> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ToolErrorKind::KernelUnavailable)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    #[cfg(unix)]
    if (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
        || metadata.mode() & 0o022 != 0
    {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    #[cfg(windows)]
    if !fence_windows::private_directory_is_hardened(path)
        .map_err(|_| ToolErrorKind::KernelUnavailable)?
    {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(())
}

pub(super) fn validate_packaged_uv(path: &Path) -> Result<(), ToolErrorKind> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ToolErrorKind::KernelUnavailable)?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_UV_EXECUTABLE_BYTES
    {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    #[cfg(unix)]
    if (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
    {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    let expected = expected_uv_sha256().ok_or(ToolErrorKind::KernelUnavailable)?;
    let mut file = File::open(path).map_err(|_| ToolErrorKind::KernelUnavailable)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes = file
            .read(&mut buffer)
            .map_err(|_| ToolErrorKind::KernelUnavailable)?;
        if bytes == 0 {
            break;
        }
        hasher.update(&buffer[..bytes]);
    }
    let mut actual = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(&mut actual, "{byte:02x}").map_err(|_| ToolErrorKind::KernelUnavailable)?;
    }
    if actual != expected {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(())
}

fn expected_uv_sha256() -> Option<&'static str> {
    let target = match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    UV_ASSETS.lines().find_map(|line| {
        let mut fields = line.split_ascii_whitespace();
        if fields.next() == Some(target) {
            let _archive_sha256 = fields.next()?;
            fields.next()
        } else {
            None
        }
    })
}

fn run_managed_process(
    mut command: Command,
    cancellation: &ProviderCancellation,
    deadline: Instant,
) -> Result<(), ToolErrorKind> {
    configure_managed_environment(&mut command);
    command.stdin(Stdio::null()).stdout(Stdio::null());
    #[cfg(test)]
    command.stderr(Stdio::inherit());
    #[cfg(not(test))]
    command.stderr(Stdio::null());
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
    wait_for_managed_process(&mut child, job, cancellation, deadline)
}

fn wait_for_managed_process(
    child: &mut Child,
    job: PlatformJob,
    cancellation: &ProviderCancellation,
    deadline: Instant,
) -> Result<(), ToolErrorKind> {
    loop {
        if cancellation.is_cancelled() {
            return if terminate_tree(child, child.id(), false, job) {
                Err(ToolErrorKind::Cancelled)
            } else {
                Err(ToolErrorKind::Uncertain)
            };
        }
        if Instant::now() >= deadline {
            return if terminate_tree(child, child.id(), false, job) {
                Err(ToolErrorKind::TimedOut)
            } else {
                Err(ToolErrorKind::Uncertain)
            };
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) => return Err(ToolErrorKind::KernelUnavailable),
            Ok(None) => thread::sleep(PROCESS_POLL_INTERVAL),
            Err(_) => {
                return if terminate_tree(child, child.id(), false, job) {
                    Err(ToolErrorKind::KernelUnavailable)
                } else {
                    Err(ToolErrorKind::Uncertain)
                };
            }
        }
    }
}

fn configure_managed_environment(command: &mut Command) {
    let inherited = env::vars_os().filter(|(name, _)| {
        let name = name.to_string_lossy().to_ascii_uppercase();
        !name.starts_with("UV_")
            && !name.starts_with("PIP_")
            && !name.starts_with("PYTHON")
            && name != "VIRTUAL_ENV"
            && name != "CONDA_PREFIX"
    });
    command.env_clear().envs(inherited);
    command
        .env("PYTHONNOUSERSITE", "1")
        .env("PYTHONSAFEPATH", "1")
        .env("UV_NO_CONFIG", "1")
        .env("UV_NO_PROJECT", "1");
}

fn ensure_private_directory(path: &Path) -> Result<(), ToolErrorKind> {
    if !path.try_exists().map_err(map_io)? {
        #[cfg(unix)]
        {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(path).map_err(map_io)?;
        }
        #[cfg(not(unix))]
        fs::create_dir(path).map_err(map_io)?;
    }
    #[cfg(windows)]
    if !fence_windows::private_directory_is_hardened(path)
        .map_err(|_| ToolErrorKind::KernelUnavailable)?
    {
        fence_windows::harden_private_directory(path)
            .map_err(|_| ToolErrorKind::KernelUnavailable)?;
    }
    validate_private_directory(path)
}

fn validate_private_directory(path: &Path) -> Result<(), ToolErrorKind> {
    let metadata = fs::symlink_metadata(path).map_err(map_io)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    #[cfg(unix)]
    {
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            return Err(ToolErrorKind::KernelUnavailable);
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(map_io)?;
    }
    #[cfg(windows)]
    if !fence_windows::private_directory_is_hardened(path)
        .map_err(|_| ToolErrorKind::KernelUnavailable)?
    {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(())
}

fn validate_managed_child_directory(path: &Path) -> Result<(), ToolErrorKind> {
    let metadata = fs::symlink_metadata(path).map_err(map_io)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(())
}

fn open_private_lock(path: &Path) -> Result<File, ToolErrorKind> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path).map_err(map_io)?;
    let metadata = file.metadata().map_err(map_io)?;
    if !metadata.file_type().is_file() {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(ToolErrorKind::KernelUnavailable);
    }
    Ok(file)
}

fn acquire_lock(
    file: &File,
    cancellation: &ProviderCancellation,
    deadline: Instant,
) -> Result<(), ToolErrorKind> {
    loop {
        if cancellation.is_cancelled() {
            return Err(ToolErrorKind::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(ToolErrorKind::TimedOut);
        }
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(TryLockError::WouldBlock) => thread::sleep(PROCESS_POLL_INTERVAL),
            Err(TryLockError::Error(_)) => return Err(ToolErrorKind::KernelUnavailable),
        }
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), ToolErrorKind> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).map_err(map_io)?;
    file.write_all(bytes).map_err(map_io)?;
    file.sync_all().map_err(map_io)
}

fn remove_managed_path(path: &Path) -> Result<(), ToolErrorKind> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(map_io(error)),
    };
    if metadata.file_type().is_symlink() || metadata.file_type().is_file() {
        fs::remove_file(path).map_err(map_io)
    } else if metadata.file_type().is_dir() {
        #[cfg(unix)]
        {
            if metadata.uid() != rustix::process::geteuid().as_raw() {
                return Err(ToolErrorKind::KernelUnavailable);
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(map_io)?;
        }
        #[cfg(windows)]
        if !fence_windows::private_directory_is_hardened(path)
            .map_err(|_| ToolErrorKind::KernelUnavailable)?
        {
            fence_windows::harden_private_directory(path)
                .map_err(|_| ToolErrorKind::KernelUnavailable)?;
        }
        fs::remove_dir_all(path).map_err(map_io)
    } else {
        Err(ToolErrorKind::KernelUnavailable)
    }
}

fn managed_tree_is_bounded(root: &Path) -> Result<bool, ToolErrorKind> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).map_err(map_io)? {
            let entry = entry.map_err(map_io)?;
            entries = entries.saturating_add(1);
            if entries > MAX_MANAGED_RUNTIME_ENTRIES {
                return Ok(false);
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(map_io)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.file_type().is_dir() {
                pending.push(entry.path());
            } else if metadata.file_type().is_file() {
                bytes = bytes.saturating_add(metadata.len());
                if bytes > MAX_MANAGED_RUNTIME_BYTES {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn path_is_inside(path: &Path, root: &Path) -> bool {
    fs::canonicalize(path)
        .ok()
        .zip(fs::canonicalize(root).ok())
        .is_some_and(|(path, root)| path.starts_with(root))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ToolErrorKind> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(map_io)
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), ToolErrorKind> {
    Ok(())
}

fn map_io(_error: io::Error) -> ToolErrorKind {
    ToolErrorKind::KernelUnavailable
}

#[cfg(test)]
fn configured_test_python() -> OsString {
    if let Some(configured) = env::var_os("MORONS_PYTHON").filter(|value| !value.is_empty()) {
        return configured;
    }
    #[cfg(windows)]
    return "python".into();
    #[cfg(not(windows))]
    "python3".into()
}

#[cfg(test)]
mod tests;
