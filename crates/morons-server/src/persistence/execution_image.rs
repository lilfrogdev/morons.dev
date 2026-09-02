use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use sha2::{Digest, Sha256};

use super::{
    ExecutionImageOutcome, ExecutionImagePlan, PersistenceError, PersistenceResourceLimit,
    paths::{
        StoragePaths, create_private_file, encode_hex, ensure_private_directory, path_entry_exists,
        sync_directory, validate_private_directory, validate_private_file,
    },
};

const STAGING_PREFIX: &str = ".provisioning-";
const CONTENT_DIRECTORY: &str = "content";
const METADATA_FILE: &str = "image-metadata";
const CARGO_DIRECTORY: &str = "cargo";
const CARGO_REGISTRY_DIRECTORY: &str = "registry";
const FORMAT_VERSION: u16 = 1;
const LIMITS_VERSION: u16 = 1;
const MAX_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 4096;
const MAX_ENTRIES: u64 = 200_000;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_LOGICAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const MANIFEST_CONTEXT: &[u8] = b"morons.dev/execution-image-manifest/v1\0";
const METADATA_CONTEXT: &[u8] = b"morons.dev/execution-image/v1\0";
const METADATA_BYTES: usize = METADATA_CONTEXT.len() + 16 + 16 + 1 + 1 + 2 + 2 + 8 + 8 + 8 + 32;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

pub(crate) enum ExecutionImageRecovery {
    Complete(ExecutionImageOutcome),
    NotApplied,
    Blocked,
}

impl StoragePaths {
    pub(crate) fn provision_execution_image(
        &self,
        plan: ExecutionImagePlan,
        toolchain_source: &str,
        cargo_source: &str,
    ) -> Result<ExecutionImageOutcome, PersistenceError> {
        let toolchain = validate_source_root(self, Path::new(toolchain_source))?;
        let cargo = validate_source_root(self, Path::new(cargo_source))?;
        if overlaps(&toolchain, &cargo) {
            return Err(invalid_source("execution image source roots overlap"));
        }
        let cargo_registry = cargo.join(CARGO_REGISTRY_DIRECTORY);
        if !ordinary_directory(&fs::symlink_metadata(&cargo_registry).map_err(|_| {
            invalid_source("the Cargo source does not contain an ordinary registry directory")
        })?) {
            return Err(invalid_source(
                "the Cargo source does not contain an ordinary registry directory",
            ));
        }

        let staging = self
            .execution_image_directory
            .join(staging_name(&plan.operation_id));
        let generation = self
            .execution_image_directory
            .join(encode_hex(&plan.generation_id));
        if path_entry_exists(&staging)? || path_entry_exists(&generation)? {
            return Err(PersistenceError::ExecutionImageBlocked);
        }
        ensure_private_directory(&staging)?;
        let content = staging.join(CONTENT_DIRECTORY);
        ensure_private_directory(&content)?;
        let result = (|| {
            let mut state = CopyState::new();
            copy_tree(&toolchain, &content, &mut Vec::new(), &mut state)?;
            validate_toolchain(&content)?;
            let cargo_destination = content.join(CARGO_DIRECTORY);
            if path_entry_exists(&cargo_destination)? {
                return Err(invalid_source(
                    "the toolchain source collides with the private Cargo seed",
                ));
            }
            ensure_private_directory(&cargo_destination)?;
            state.reserve_entry()?;
            state.record_directory(b"cargo")?;
            let registry_destination = cargo_destination.join(CARGO_REGISTRY_DIRECTORY);
            ensure_private_directory(&registry_destination)?;
            state.reserve_entry()?;
            state.record_directory(b"cargo/registry")?;
            let mut components = vec![
                CARGO_DIRECTORY.to_owned(),
                CARGO_REGISTRY_DIRECTORY.to_owned(),
            ];
            copy_tree(
                &cargo_registry,
                &registry_destination,
                &mut components,
                &mut state,
            )?;
            sync_directory(&registry_destination)?;
            sync_directory(&cargo_destination)?;
            sync_directory(&content)?;
            let outcome = state.finish();
            write_metadata(&staging, plan, outcome)?;
            sync_directory(&staging)?;
            fs::rename(&staging, &generation)?;
            sync_directory(&self.execution_image_directory)?;
            Ok(outcome)
        })();
        if result.is_err() {
            if path_entry_exists(&generation).unwrap_or(true) {
                return Err(PersistenceError::ExecutionImageBlocked);
            }
            if path_entry_exists(&staging).unwrap_or(true)
                && remove_confined_tree(&self.execution_image_directory, &staging).is_err()
            {
                return Err(PersistenceError::ExecutionImageBlocked);
            }
        }
        result
    }

    pub(crate) fn recover_execution_image(
        &self,
        plan: ExecutionImagePlan,
    ) -> Result<ExecutionImageRecovery, PersistenceError> {
        let staging = self
            .execution_image_directory
            .join(staging_name(&plan.operation_id));
        let generation = self
            .execution_image_directory
            .join(encode_hex(&plan.generation_id));
        match (
            path_entry_exists(&staging)?,
            path_entry_exists(&generation)?,
        ) {
            (true, true) => Ok(ExecutionImageRecovery::Blocked),
            (false, true) => validate_generation(&generation, plan)
                .map(ExecutionImageRecovery::Complete)
                .or(Ok(ExecutionImageRecovery::Blocked)),
            (true, false) => {
                if !path_entry_exists(&staging.join(METADATA_FILE))? {
                    remove_confined_tree(&self.execution_image_directory, &staging)?;
                    return Ok(ExecutionImageRecovery::NotApplied);
                }
                let outcome = match validate_generation(&staging, plan) {
                    Ok(outcome) => outcome,
                    Err(_) => return Ok(ExecutionImageRecovery::Blocked),
                };
                fs::rename(&staging, &generation)?;
                sync_directory(&self.execution_image_directory)?;
                Ok(ExecutionImageRecovery::Complete(outcome))
            }
            (false, false) => Ok(ExecutionImageRecovery::NotApplied),
        }
    }

    pub(crate) fn validate_execution_image(
        &self,
        plan: ExecutionImagePlan,
        expected: ExecutionImageOutcome,
    ) -> Result<(), PersistenceError> {
        let generation = self
            .execution_image_directory
            .join(encode_hex(&plan.generation_id));
        let actual = validate_generation(&generation, plan)?;
        if actual != expected {
            return Err(invalid_image());
        }
        Ok(())
    }

    pub(crate) fn cleanup_inactive_execution_images(
        &self,
        active_generation: Option<[u8; 16]>,
    ) -> Result<(), PersistenceError> {
        let active = active_generation.map(|identifier| encode_hex(&identifier));
        let mut removed = false;
        for entry in fs::read_dir(&self.execution_image_directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name_text) = name.to_str() else {
                return Err(invalid_image());
            };
            if active.as_deref() == Some(name_text) {
                continue;
            }
            if is_generation_name(name_text) || is_staging_name(name_text) {
                validate_private_directory(&entry.path())?;
                fs::remove_dir_all(entry.path())?;
                removed = true;
            } else {
                return Err(invalid_image());
            }
        }
        if removed {
            sync_directory(&self.execution_image_directory)?;
        }
        Ok(())
    }
}

fn validate_source_root(paths: &StoragePaths, source: &Path) -> Result<PathBuf, PersistenceError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|_| invalid_source("an execution image source root is unavailable"))?;
    if !ordinary_directory(&metadata) {
        return Err(invalid_source(
            "an execution image source root is not an ordinary directory",
        ));
    }
    let source = fs::canonicalize(source)
        .map_err(|_| invalid_source("an execution image source root could not be resolved"))?;
    let application = fs::canonicalize(paths.application_directory()).map_err(|_| {
        PersistenceError::InvalidState {
            reason: "the application root could not be resolved",
        }
    })?;
    if overlaps(&source, &application) {
        return Err(invalid_source(
            "an execution image source overlaps protected Morons state",
        ));
    }
    Ok(source)
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    components: &mut Vec<String>,
    state: &mut CopyState,
) -> Result<(), PersistenceError> {
    if components.len() > MAX_DEPTH {
        return Err(limit());
    }
    let before = fs::symlink_metadata(source).map_err(|_| changed_source())?;
    if !ordinary_directory(&before) {
        return Err(changed_source());
    }
    let names_before = snapshot_entries(source)?;
    let mut entries = fs::read_dir(source)
        .map_err(|_| changed_source())?
        .map(|entry| entry.map_err(|_| changed_source()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| changed_source())?;
        validate_component(&name)?;
        if name.eq_ignore_ascii_case(".git") {
            continue;
        }
        components.push(name.clone());
        let relative = relative_bytes(components)?;
        state.reserve_entry()?;
        let source_path = source.join(&name);
        let destination_path = destination.join(&name);
        let metadata = fs::symlink_metadata(&source_path).map_err(|_| changed_source())?;
        if ordinary_directory(&metadata) {
            ensure_private_directory(&destination_path)?;
            state.record_directory(&relative)?;
            copy_tree(&source_path, &destination_path, components, state)?;
            sync_directory(&destination_path)?;
        } else if ordinary_file(&metadata) {
            copy_file(&source_path, &destination_path, &relative, &metadata, state)?;
        } else {
            return Err(changed_source());
        }
        components.pop();
    }
    let after = fs::symlink_metadata(source).map_err(|_| changed_source())?;
    if !same_identity(&before, &after) || names_before != snapshot_entries(source)? {
        return Err(changed_source());
    }
    Ok(())
}

fn snapshot_entries(path: &Path) -> Result<Vec<Vec<u8>>, PersistenceError> {
    let mut names = fs::read_dir(path)
        .map_err(|_| changed_source())?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().as_encoded_bytes().to_vec())
                .map_err(|_| changed_source())
        })
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names)
}

fn copy_file(
    source: &Path,
    destination: &Path,
    relative: &[u8],
    expected: &fs::Metadata,
    state: &mut CopyState,
) -> Result<(), PersistenceError> {
    let size = expected.len();
    state.reserve_bytes(size)?;
    if size > MAX_FILE_BYTES {
        return Err(limit());
    }
    let mut input = File::open(source).map_err(|_| changed_source())?;
    let opened = input.metadata().map_err(|_| changed_source())?;
    if !ordinary_file(&opened) || !same_identity(expected, &opened) || opened.len() != size {
        return Err(changed_source());
    }
    let mut output = create_private_file(destination)?;
    let mut content = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = input.read(&mut buffer).map_err(|_| changed_source())?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| limit())?)
            .ok_or_else(limit)?;
        if copied > size {
            return Err(changed_source());
        }
        content.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    buffer.fill(0);
    let after_handle = input.metadata().map_err(|_| changed_source())?;
    let after_path = fs::symlink_metadata(source).map_err(|_| changed_source())?;
    if copied != size
        || !same_identity(expected, &after_handle)
        || !same_identity(expected, &after_path)
        || after_handle.len() != size
        || modified_value(expected) != modified_value(&after_handle)
    {
        return Err(changed_source());
    }
    output.sync_all()?;
    drop(output);
    let executable = source_is_executable(expected);
    #[cfg(unix)]
    if executable {
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
        File::open(destination)?.sync_all()?;
    }
    let digest: [u8; 32] = content.finalize().into();
    state.record_file(relative, size, executable, &digest)?;
    Ok(())
}

fn validate_toolchain(content: &Path) -> Result<(), PersistenceError> {
    #[cfg(windows)]
    let tools = ["bin/cargo.exe", "bin/rustc.exe"];
    #[cfg(not(windows))]
    let tools = ["bin/cargo", "bin/rustc"];
    for relative in tools {
        let metadata = fs::symlink_metadata(content.join(relative))
            .map_err(|_| invalid_source("the toolchain source does not contain cargo and rustc"))?;
        if !ordinary_file(&metadata) || !source_is_executable(&metadata) {
            return Err(invalid_source(
                "the toolchain source does not contain executable cargo and rustc files",
            ));
        }
    }
    Ok(())
}

fn validate_generation(
    root: &Path,
    plan: ExecutionImagePlan,
) -> Result<ExecutionImageOutcome, PersistenceError> {
    validate_private_directory(root)?;
    let expected = read_metadata(root, plan)?;
    let content = root.join(CONTENT_DIRECTORY);
    validate_private_directory(&content)?;
    validate_toolchain(&content).map_err(|_| invalid_image())?;
    let mut state = ScanState::new();
    scan_tree(&content, &mut Vec::new(), &mut state)?;
    let actual = state.finish();
    if actual != expected {
        return Err(invalid_image());
    }
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        if name != OsStr::new(CONTENT_DIRECTORY) && name != OsStr::new(METADATA_FILE) {
            return Err(invalid_image());
        }
    }
    Ok(actual)
}

fn scan_tree(
    directory: &Path,
    components: &mut Vec<String>,
    state: &mut ScanState,
) -> Result<(), PersistenceError> {
    let metadata = fs::symlink_metadata(directory)?;
    if !ordinary_directory(&metadata) || components.len() > MAX_DEPTH {
        return Err(invalid_image());
    }
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_image())?;
        validate_component(&name).map_err(|_| invalid_image())?;
        components.push(name);
        let relative = relative_bytes(components).map_err(|_| invalid_image())?;
        state.reserve_entry()?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if ordinary_directory(&metadata) {
            state.record_directory(&relative)?;
            scan_tree(&path, components, state)?;
        } else if ordinary_file(&metadata) {
            let (digest, size) = hash_file(&path)?;
            state.reserve_bytes(size)?;
            state.record_file(&relative, size, source_is_executable(&metadata), &digest)?;
        } else {
            return Err(invalid_image());
        }
        components.pop();
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<([u8; 32], u64), PersistenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if !ordinary_file(&metadata) || metadata.len() > MAX_FILE_BYTES {
        return Err(invalid_image());
    }
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).map_err(|_| limit())?)
            .ok_or_else(limit)?;
        if size > MAX_FILE_BYTES {
            return Err(limit());
        }
        digest.update(&buffer[..read]);
    }
    buffer.fill(0);
    Ok((digest.finalize().into(), size))
}

fn write_metadata(
    root: &Path,
    plan: ExecutionImagePlan,
    outcome: ExecutionImageOutcome,
) -> Result<(), PersistenceError> {
    let mut file = create_private_file(&root.join(METADATA_FILE))?;
    file.write_all(METADATA_CONTEXT)?;
    file.write_all(&plan.generation_id)?;
    file.write_all(&plan.operation_id)?;
    file.write_all(&[u8::try_from(plan.target_os.to_record()).map_err(|_| invalid_image())?])?;
    file.write_all(&[u8::try_from(plan.target_arch.to_record()).map_err(|_| invalid_image())?])?;
    file.write_all(&FORMAT_VERSION.to_be_bytes())?;
    file.write_all(&LIMITS_VERSION.to_be_bytes())?;
    file.write_all(&outcome.file_count.to_be_bytes())?;
    file.write_all(&outcome.directory_count.to_be_bytes())?;
    file.write_all(&outcome.logical_bytes.to_be_bytes())?;
    file.write_all(&outcome.manifest_digest)?;
    file.sync_all()?;
    validate_private_file(&root.join(METADATA_FILE), Some(METADATA_BYTES as u64))?;
    Ok(())
}

fn read_metadata(
    root: &Path,
    plan: ExecutionImagePlan,
) -> Result<ExecutionImageOutcome, PersistenceError> {
    let path = root.join(METADATA_FILE);
    validate_private_file(&path, Some(METADATA_BYTES as u64))?;
    let mut bytes = vec![0_u8; METADATA_BYTES];
    File::open(path)?.read_exact(&mut bytes)?;
    let mut offset = METADATA_CONTEXT.len();
    if &bytes[..offset] != METADATA_CONTEXT
        || bytes[offset..offset + 16] != plan.generation_id
        || bytes[offset + 16..offset + 32] != plan.operation_id
    {
        return Err(invalid_image());
    }
    offset += 32;
    if bytes[offset] != plan.target_os.to_record() as u8
        || bytes[offset + 1] != plan.target_arch.to_record() as u8
    {
        return Err(invalid_image());
    }
    offset += 2;
    if take_u16(&bytes, &mut offset)? != FORMAT_VERSION
        || take_u16(&bytes, &mut offset)? != LIMITS_VERSION
    {
        return Err(invalid_image());
    }
    let file_count = take_u64(&bytes, &mut offset)?;
    let directory_count = take_u64(&bytes, &mut offset)?;
    let logical_bytes = take_u64(&bytes, &mut offset)?;
    let manifest_digest = bytes[offset..offset + 32]
        .try_into()
        .map_err(|_| invalid_image())?;
    Ok(ExecutionImageOutcome {
        file_count,
        directory_count,
        logical_bytes,
        manifest_digest,
    })
}

fn take_u16(bytes: &[u8], offset: &mut usize) -> Result<u16, PersistenceError> {
    let end = offset.checked_add(2).ok_or_else(invalid_image)?;
    let value = u16::from_be_bytes(
        bytes[*offset..end]
            .try_into()
            .map_err(|_| invalid_image())?,
    );
    *offset = end;
    Ok(value)
}

fn take_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, PersistenceError> {
    let end = offset.checked_add(8).ok_or_else(invalid_image)?;
    let value = u64::from_be_bytes(
        bytes[*offset..end]
            .try_into()
            .map_err(|_| invalid_image())?,
    );
    *offset = end;
    Ok(value)
}

struct CopyState {
    manifest: Sha256,
    file_count: u64,
    directory_count: u64,
    logical_bytes: u64,
}

impl CopyState {
    fn new() -> Self {
        let mut manifest = Sha256::new();
        manifest.update(MANIFEST_CONTEXT);
        Self {
            manifest,
            file_count: 0,
            directory_count: 0,
            logical_bytes: 0,
        }
    }

    fn reserve_entry(&self) -> Result<(), PersistenceError> {
        if self.file_count + self.directory_count >= MAX_ENTRIES {
            return Err(limit());
        }
        Ok(())
    }

    fn reserve_bytes(&self, bytes: u64) -> Result<(), PersistenceError> {
        if self
            .logical_bytes
            .checked_add(bytes)
            .is_none_or(|total| total > MAX_LOGICAL_BYTES)
        {
            return Err(limit());
        }
        Ok(())
    }

    fn record_directory(&mut self, relative: &[u8]) -> Result<(), PersistenceError> {
        self.manifest.update([0]);
        self.manifest.update(path_length(relative)?.to_be_bytes());
        self.manifest.update(relative);
        self.directory_count += 1;
        Ok(())
    }

    fn record_file(
        &mut self,
        relative: &[u8],
        size: u64,
        executable: bool,
        content: &[u8; 32],
    ) -> Result<(), PersistenceError> {
        self.manifest.update([1]);
        self.manifest.update(path_length(relative)?.to_be_bytes());
        self.manifest.update(relative);
        self.manifest.update([u8::from(executable)]);
        self.manifest.update(size.to_be_bytes());
        self.manifest.update(content);
        self.file_count += 1;
        self.logical_bytes += size;
        Ok(())
    }

    fn finish(self) -> ExecutionImageOutcome {
        ExecutionImageOutcome {
            file_count: self.file_count,
            directory_count: self.directory_count,
            logical_bytes: self.logical_bytes,
            manifest_digest: self.manifest.finalize().into(),
        }
    }
}

type ScanState = CopyState;

fn relative_bytes(components: &[String]) -> Result<Vec<u8>, PersistenceError> {
    let value = components.join("/").into_bytes();
    if value.len() > MAX_PATH_BYTES {
        return Err(limit());
    }
    Ok(value)
}

fn path_length(path: &[u8]) -> Result<u32, PersistenceError> {
    u32::try_from(path.len()).map_err(|_| limit())
}

fn validate_component(name: &str) -> Result<(), PersistenceError> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['\0', '\\', ':']) {
        return Err(changed_source());
    }
    Ok(())
}

fn staging_name(operation_id: &[u8; 16]) -> String {
    format!("{STAGING_PREFIX}{}", encode_hex(operation_id))
}

fn is_staging_name(name: &str) -> bool {
    name.strip_prefix(STAGING_PREFIX)
        .is_some_and(is_generation_name)
}

fn is_generation_name(name: &str) -> bool {
    name.len() == 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn remove_confined_tree(parent: &Path, path: &Path) -> Result<(), PersistenceError> {
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    if path.parent() != Some(parent) || !is_staging_name(name) {
        return Err(invalid_image());
    }
    validate_private_directory(path)?;
    fs::remove_dir_all(path)?;
    sync_directory(parent)?;
    Ok(())
}

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn ordinary_file(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_file() && !is_reparse(metadata)
}

fn ordinary_directory(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_dir() && !metadata.file_type().is_symlink() && !is_reparse(metadata)
}

#[cfg(unix)]
fn is_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(unix)]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino() && left.file_type() == right.file_type()
}

#[cfg(windows)]
fn same_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.volume_serial_number() == right.volume_serial_number()
        && left.file_index() == right.file_index()
        && left.file_type() == right.file_type()
}

#[cfg(unix)]
fn modified_value(metadata: &fs::Metadata) -> Option<std::time::SystemTime> {
    metadata.modified().ok()
}

#[cfg(windows)]
fn modified_value(metadata: &fs::Metadata) -> Option<std::time::SystemTime> {
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_nanos(metadata.last_write_time() * 100))
}

#[cfg(unix)]
fn source_is_executable(metadata: &fs::Metadata) -> bool {
    metadata.mode() & 0o111 != 0
}

#[cfg(windows)]
fn source_is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn invalid_source(reason: &'static str) -> PersistenceError {
    PersistenceError::InvalidInput { reason }
}

fn changed_source() -> PersistenceError {
    invalid_source("an execution image source changed or contains unsupported state")
}

fn invalid_image() -> PersistenceError {
    PersistenceError::InvalidState {
        reason: "execution image state is invalid",
    }
}

fn limit() -> PersistenceError {
    PersistenceError::ResourceLimit {
        resource: PersistenceResourceLimit::ExecutionImage,
    }
}
