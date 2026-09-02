use std::{
    fs,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use crate::{SANDBOX_PROTOCOL_VERSION, SandboxRequest, SandboxResult, SandboxStatus};

const MAX_ROOT_BYTES: usize = 4096;
const MAX_RELATIVE_PATH_BYTES: usize = 1024;
const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 4096;
const MAX_ARGUMENT_TOTAL_BYTES: usize = 64 * 1024;
const MIN_WALL_TIME_MILLISECONDS: u64 = 100;
const MAX_WALL_TIME_MILLISECONDS: u64 = 30 * 60 * 1000;
const MIN_OUTPUT_BYTES: u32 = 1024;
const MAX_OUTPUT_BYTES: u32 = 256 * 1024;
const MAX_CARGO_SEED_ENTRIES: u64 = 200_000;
const MAX_CARGO_SEED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const CARGO_COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub(crate) struct PreparedRequest {
    pub operation_id: [u8; 16],
    pub candidate_root: PathBuf,
    pub scratch_root: PathBuf,
    pub image_root: PathBuf,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub working_directory: PathBuf,
    pub wall_time_milliseconds: u64,
    pub output_bytes_per_stream: usize,
}

pub fn execute(request: SandboxRequest, cancellation: &Cancellation) -> SandboxResult {
    let operation_id = request.operation_id;
    let prepared = match validate_request(&request) {
        Ok(prepared) => prepared,
        Err(()) => {
            return SandboxResult::failure(operation_id, SandboxStatus::RequestRejected);
        }
    };
    if cancellation.is_cancelled() {
        return SandboxResult::failure(operation_id, SandboxStatus::Cancelled);
    }

    #[cfg(target_os = "macos")]
    {
        crate::macos::execute(prepared, cancellation)
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::execute(prepared, request, cancellation)
    }
    #[cfg(target_os = "windows")]
    {
        crate::windows::execute(prepared, request, cancellation)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let PreparedRequest {
            operation_id: prepared_operation_id,
            candidate_root,
            scratch_root,
            image_root,
            executable,
            arguments,
            working_directory,
            wall_time_milliseconds,
            output_bytes_per_stream,
        } = prepared;
        drop((
            candidate_root,
            scratch_root,
            image_root,
            executable,
            arguments,
            working_directory,
            wall_time_milliseconds,
            output_bytes_per_stream,
        ));
        SandboxResult::failure(prepared_operation_id, SandboxStatus::BackendUnavailable)
    }
}

pub(crate) fn validate_request(request: &SandboxRequest) -> Result<PreparedRequest, ()> {
    if request.protocol_version != SANDBOX_PROTOCOL_VERSION
        || request.operation_id.iter().all(|byte| *byte == 0)
        || request.arguments.len() > MAX_ARGUMENTS
        || !(MIN_WALL_TIME_MILLISECONDS..=MAX_WALL_TIME_MILLISECONDS)
            .contains(&request.limits.wall_time_milliseconds)
        || !(MIN_OUTPUT_BYTES..=MAX_OUTPUT_BYTES).contains(&request.limits.output_bytes_per_stream)
        || !valid_relative_path(&request.executable, false)
        || !valid_relative_path(&request.working_directory, true)
    {
        return Err(());
    }

    let argument_total = request
        .arguments
        .iter()
        .try_fold(0_usize, |total, argument| {
            if argument.len() > MAX_ARGUMENT_BYTES || argument.contains('\0') {
                return None;
            }
            total.checked_add(argument.len())
        });
    if argument_total.is_none_or(|total| total > MAX_ARGUMENT_TOTAL_BYTES) {
        return Err(());
    }

    let candidate_root = validate_root(&request.candidate_root)?;
    let scratch_root = validate_root(&request.scratch_root)?;
    let image_root = validate_root(&request.image_root)?;
    if overlaps(&candidate_root, &scratch_root)
        || overlaps(&candidate_root, &image_root)
        || overlaps(&scratch_root, &image_root)
    {
        return Err(());
    }

    let executable = validate_descendant(&image_root, &request.executable, false)?;
    let working_directory = if request.working_directory == "." {
        candidate_root.clone()
    } else {
        validate_descendant(&candidate_root, &request.working_directory, true)?
    };

    Ok(PreparedRequest {
        operation_id: request.operation_id,
        candidate_root,
        scratch_root,
        image_root,
        executable,
        arguments: request.arguments.clone(),
        working_directory,
        wall_time_milliseconds: request.limits.wall_time_milliseconds,
        output_bytes_per_stream: request.limits.output_bytes_per_stream as usize,
    })
}

fn validate_root(value: &str) -> Result<PathBuf, ()> {
    if value.is_empty() || value.len() > MAX_ROOT_BYTES || value.contains('\0') {
        return Err(());
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(());
    }
    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(());
    }
    let canonical = fs::canonicalize(path).map_err(|_| ())?;
    if canonical.as_os_str().as_encoded_bytes().len() > MAX_ROOT_BYTES {
        return Err(());
    }
    Ok(canonical)
}

fn validate_descendant(root: &Path, relative: &str, directory: bool) -> Result<PathBuf, ()> {
    let mut path = root.to_path_buf();
    for component in relative.split('/') {
        path.push(component);
        let metadata = fs::symlink_metadata(&path).map_err(|_| ())?;
        if metadata.file_type().is_symlink() {
            return Err(());
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| ())?;
    if directory {
        if !metadata.file_type().is_dir() {
            return Err(());
        }
    } else if !metadata.file_type().is_file() {
        return Err(());
    }
    #[cfg(unix)]
    if !directory && metadata.permissions().mode() & 0o111 == 0 {
        return Err(());
    }
    let canonical = fs::canonicalize(&path).map_err(|_| ())?;
    if !canonical.starts_with(root) {
        return Err(());
    }
    Ok(canonical)
}

fn valid_relative_path(value: &str, root_allowed: bool) -> bool {
    if value == "." {
        return root_allowed;
    }
    if value.is_empty()
        || value.len() > MAX_RELATIVE_PATH_BYTES
        || value.contains(['\0', '\\', ':'])
    {
        return false;
    }
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(name) if !name.is_empty()))
        && value.split('/').all(|component| !component.is_empty())
}

pub(crate) fn seed_cargo_home(image: &Path, destination: &Path) -> Result<(), ()> {
    let source = image.join("cargo");
    let source_metadata = fs::symlink_metadata(&source).map_err(|_| ())?;
    let destination_metadata = fs::symlink_metadata(destination).map_err(|_| ())?;
    if !source_metadata.file_type().is_dir()
        || source_metadata.file_type().is_symlink()
        || metadata_is_reparse(&source_metadata)
        || !destination_metadata.file_type().is_dir()
        || destination_metadata.file_type().is_symlink()
        || metadata_is_reparse(&destination_metadata)
        || fs::read_dir(destination).map_err(|_| ())?.next().is_some()
    {
        return Err(());
    }
    let mut state = CargoSeedState::default();
    copy_cargo_seed(&source, destination, 0, &mut state)
}

#[derive(Default)]
struct CargoSeedState {
    entries: u64,
    bytes: u64,
}

fn copy_cargo_seed(
    source: &Path,
    destination: &Path,
    depth: usize,
    state: &mut CargoSeedState,
) -> Result<(), ()> {
    if depth > 128 {
        return Err(());
    }
    let mut entries = fs::read_dir(source)
        .map_err(|_| ())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if name.to_str().is_none() {
            return Err(());
        }
        state.entries = state.entries.checked_add(1).ok_or(())?;
        if state.entries > MAX_CARGO_SEED_ENTRIES {
            return Err(());
        }
        let source_path = entry.path();
        let destination_path = destination.join(&name);
        let metadata = fs::symlink_metadata(&source_path).map_err(|_| ())?;
        if metadata.file_type().is_symlink() || metadata_is_reparse(&metadata) {
            return Err(());
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path).map_err(|_| ())?;
            copy_cargo_seed(&source_path, &destination_path, depth + 1, state)?;
        } else if metadata.is_file() {
            state.bytes = state.bytes.checked_add(metadata.len()).ok_or(())?;
            if state.bytes > MAX_CARGO_SEED_BYTES {
                return Err(());
            }
            copy_seed_file(&source_path, &destination_path, metadata.len())?;
        } else {
            return Err(());
        }
    }
    Ok(())
}

fn copy_seed_file(source: &Path, destination: &Path, expected: u64) -> Result<(), ()> {
    let mut input = File::open(source).map_err(|_| ())?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|_| ())?;
    let mut copied = 0_u64;
    let mut buffer = [0_u8; CARGO_COPY_BUFFER_BYTES];
    loop {
        let read = input.read(&mut buffer).map_err(|_| ())?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| ())?)
            .ok_or(())?;
        if copied > expected {
            return Err(());
        }
        output.write_all(&buffer[..read]).map_err(|_| ())?;
    }
    output.sync_all().map_err(|_| ())?;
    let after = fs::symlink_metadata(source).map_err(|_| ())?;
    if copied != expected
        || !after.is_file()
        || after.file_type().is_symlink()
        || metadata_is_reparse(&after)
        || after.len() != expected
    {
        return Err(());
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use std::{fs::File, io::Write};

    use super::*;
    use crate::SandboxLimits;

    struct Roots {
        parent: PathBuf,
        candidate: PathBuf,
        scratch: PathBuf,
        image: PathBuf,
    }

    impl Roots {
        fn new() -> Self {
            let mut identifier = [0_u8; 16];
            getrandom::fill(&mut identifier).expect("test randomness");
            let parent = std::env::temp_dir().join(format!(
                "morons-sandbox-test-{}",
                identifier
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ));
            let candidate = parent.join("candidate");
            let scratch = parent.join("scratch");
            let image = parent.join("image");
            for path in [
                &parent,
                &candidate,
                &scratch,
                &image,
                &image.join("bin"),
                &image.join("cargo"),
            ] {
                create_private_directory(path);
            }
            let executable = image.join("bin/tool");
            let mut file = File::create(&executable).expect("creates executable");
            file.write_all(b"fixture").expect("writes executable");
            #[cfg(unix)]
            fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
                .expect("sets executable mode");
            Self {
                parent,
                candidate,
                scratch,
                image,
            }
        }

        fn request(&self) -> SandboxRequest {
            SandboxRequest {
                protocol_version: SANDBOX_PROTOCOL_VERSION,
                operation_id: [1; 16],
                candidate_root: self.candidate.to_string_lossy().into_owned(),
                scratch_root: self.scratch.to_string_lossy().into_owned(),
                image_root: self.image.to_string_lossy().into_owned(),
                executable: "bin/tool".to_owned(),
                arguments: vec!["check".to_owned()],
                working_directory: ".".to_owned(),
                limits: SandboxLimits {
                    wall_time_milliseconds: 1_000,
                    output_bytes_per_stream: 4_096,
                },
            }
        }
    }

    impl Drop for Roots {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }

    #[test]
    fn request_validation_confines_roots_program_and_working_directory() {
        let roots = Roots::new();
        assert!(validate_request(&roots.request()).is_ok());

        let mut escaping = roots.request();
        escaping.executable = "../tool".to_owned();
        assert!(validate_request(&escaping).is_err());

        let mut overlapping = roots.request();
        overlapping.scratch_root = roots.candidate.to_string_lossy().into_owned();
        assert!(validate_request(&overlapping).is_err());

        let mut zero = roots.request();
        zero.operation_id = [0; 16];
        assert!(validate_request(&zero).is_err());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    #[test]
    fn unsupported_native_backends_fail_closed_after_validation() {
        let roots = Roots::new();
        let result = execute(roots.request(), &Cancellation::new());
        assert_eq!(result.status, SandboxStatus::BackendUnavailable);
        assert!(!result.candidate_eligible);
    }

    #[test]
    fn cargo_seed_is_copied_privately_without_sharing_writes() {
        let roots = Roots::new();
        let registry = roots.image.join("cargo/registry/cache");
        create_private_directory(&roots.image.join("cargo/registry"));
        create_private_directory(&registry);
        fs::write(registry.join("fixture.crate"), b"immutable-seed").expect("writes seed");
        let destination = roots.scratch.join("seeded-cargo");
        create_private_directory(&destination);
        seed_cargo_home(&roots.image, &destination).expect("seeds Cargo home");
        fs::write(
            destination.join("registry/cache/fixture.crate"),
            b"operation-write",
        )
        .expect("private seed should be writable");
        assert_eq!(
            fs::read(registry.join("fixture.crate")).expect("source remains readable"),
            b"immutable-seed"
        );
    }

    #[test]
    fn request_debug_never_exposes_validated_host_roots() {
        let roots = Roots::new();
        let debug = format!("{:?}", roots.request());
        assert!(!debug.contains(&roots.parent.to_string_lossy().into_owned()));
    }

    fn create_private_directory(path: &Path) {
        fs::create_dir(path).expect("creates private directory");
        #[cfg(unix)]
        fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .expect("sets private mode");
    }
}
