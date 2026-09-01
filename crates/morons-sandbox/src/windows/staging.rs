use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};

use rappct::{
    AppContainerProfile, SecurityCapabilities, SecurityCapabilitiesBuilder,
    acl::{AccessMask, ResourcePath, grant_to_package},
};

use crate::{SANDBOX_PROTOCOL_VERSION, SandboxRequest, runner::PreparedRequest};

const MAX_IMAGE_ENTRIES: u64 = 200_000;
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_COPY_BUFFER_BYTES: usize = 64 * 1024;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const FILE_GENERIC_READ: u32 = 0x0012_0089;
const FILE_GENERIC_EXECUTE: u32 = 0x0012_00a0;
const FILE_ALL_ACCESS: u32 = 0x001f_01ff;

pub(super) struct Layout {
    pub(super) root: PathBuf,
    pub(super) image: PathBuf,
    pub(super) runner: PathBuf,
    pub(super) runtime: PathBuf,
    pub(super) home: PathBuf,
    pub(super) temporary: PathBuf,
    pub(super) cargo_home: PathBuf,
}

impl Layout {
    pub(super) fn prepare(request: &PreparedRequest) -> Result<Self, ()> {
        let root = request.scratch_root.join(format!(
            ".morons-windows-{}",
            hexadecimal(&request.operation_id)
        ));
        fs::create_dir(&root).map_err(|_| ())?;
        let image = root.join("image");
        let runner = root.join("runner.exe");
        let runtime = root.join("runtime");
        let home = runtime.join("home");
        let temporary = runtime.join("tmp");
        let cargo_home = runtime.join("cargo-home");
        let prepared = (|| {
            for directory in [&image, &runtime, &home, &temporary, &cargo_home] {
                fs::create_dir(directory).map_err(|_| ())?;
            }
            copy_image(&request.image_root, &image)?;
            copy_runner(&runner)?;
            Ok(Self {
                root: root.clone(),
                image,
                runner,
                runtime,
                home,
                temporary,
                cargo_home,
            })
        })();
        if prepared.is_err() {
            let _ = remove_tree(&root);
        }
        prepared
    }

    pub(super) fn stage_request(
        &self,
        prepared: &PreparedRequest,
        original: &SandboxRequest,
    ) -> Result<SandboxRequest, ()> {
        Ok(SandboxRequest {
            protocol_version: SANDBOX_PROTOCOL_VERSION,
            operation_id: original.operation_id,
            candidate_root: utf8(&prepared.candidate_root)?,
            scratch_root: utf8(&self.runtime)?,
            image_root: utf8(&self.image)?,
            executable: original.executable.clone(),
            arguments: original.arguments.clone(),
            working_directory: original.working_directory.clone(),
            limits: original.limits,
        })
    }

    pub(super) fn cleanup(&self) -> Result<(), ()> {
        remove_tree(&self.root)
    }
}

pub(super) struct Container {
    profile: Option<AppContainerProfile>,
}

impl Container {
    pub(super) fn create(operation_id: [u8; 16]) -> Result<Self, ()> {
        let name = format!("morons-{}", hexadecimal(&operation_id));
        let profile = AppContainerProfile::ensure(&name, "Morons sandbox", Some("Morons sandbox"))
            .map_err(|_| ())?;
        Ok(Self {
            profile: Some(profile),
        })
    }

    pub(super) fn profile(&self) -> Result<&AppContainerProfile, ()> {
        self.profile.as_ref().ok_or(())
    }

    pub(super) fn grant_paths(
        &self,
        prepared: &PreparedRequest,
        layout: &Layout,
    ) -> Result<(), ()> {
        let profile = self.profile()?;
        grant_directory(&prepared.candidate_root, profile, FILE_ALL_ACCESS)?;
        for directory in [&layout.home, &layout.temporary, &layout.cargo_home] {
            grant_directory(directory, profile, FILE_ALL_ACCESS)?;
        }
        grant_directory(
            &layout.image,
            profile,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        )?;
        grant_file(
            &layout.runner,
            profile,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        )?;
        Ok(())
    }

    pub(super) fn capabilities(&self) -> Result<SecurityCapabilities, ()> {
        SecurityCapabilitiesBuilder::new(&self.profile()?.sid)
            .build()
            .map_err(|_| ())
    }

    pub(super) fn delete(mut self) -> Result<(), ()> {
        self.profile.take().ok_or(())?.delete().map_err(|_| ())
    }
}

pub(super) fn bootstrap_environment(layout: &Layout) -> Result<Vec<(OsString, OsString)>, ()> {
    let mut environment = Vec::new();
    for name in [
        "SystemRoot",
        "windir",
        "SystemDrive",
        "ComSpec",
        "PATHEXT",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "ALLUSERSPROFILE",
    ] {
        let value = std::env::var_os(name).ok_or(())?;
        environment.push((OsString::from(name), value));
    }
    for name in [
        "OS",
        "PROCESSOR_ARCHITECTURE",
        "PROCESSOR_IDENTIFIER",
        "PROCESSOR_LEVEL",
        "PROCESSOR_REVISION",
        "NUMBER_OF_PROCESSORS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            environment.push((OsString::from(name), value));
        }
    }
    environment.push((OsString::from("TEMP"), layout.temporary.as_os_str().into()));
    environment.push((OsString::from("TMP"), layout.temporary.as_os_str().into()));
    if let Some(value) = std::env::var_os("MORONS_SANDBOX_TEST_DIAGNOSTICS") {
        environment.push((OsString::from("MORONS_SANDBOX_TEST_DIAGNOSTICS"), value));
    }
    environment.sort_by_cached_key(|(name, _)| name.to_string_lossy().to_ascii_lowercase());
    Ok(environment)
}

pub(super) fn launch_path(path: &Path) -> Result<PathBuf, ()> {
    let value = path.to_str().ok_or(())?;
    Ok(value
        .strip_prefix(r"\\?\")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf()))
}

pub(super) fn command_line(executable: &Path, mode: &str) -> Result<String, ()> {
    let executable = executable.to_str().ok_or(())?;
    if executable.contains(['\0', '"']) || mode.contains(['\0', '"', ' ', '\t']) {
        return Err(());
    }
    Ok(format!("\"{executable}\" {mode}"))
}

fn grant_directory(path: &Path, profile: &AppContainerProfile, access: u32) -> Result<(), ()> {
    grant_to_package(
        ResourcePath::Directory(path.to_path_buf()),
        &profile.sid,
        AccessMask(access),
    )
    .map_err(|_| ())
}

fn grant_file(path: &Path, profile: &AppContainerProfile, access: u32) -> Result<(), ()> {
    grant_to_package(
        ResourcePath::File(path.to_path_buf()),
        &profile.sid,
        AccessMask(access),
    )
    .map_err(|_| ())
}

fn copy_image(source: &Path, destination: &Path) -> Result<(), ()> {
    let mut state = CopyState::default();
    copy_directory_contents(source, destination, 0, &mut state)
}

#[derive(Default)]
struct CopyState {
    entries: u64,
    bytes: u64,
}

fn copy_directory_contents(
    source: &Path,
    destination: &Path,
    depth: usize,
    state: &mut CopyState,
) -> Result<(), ()> {
    if depth > 128 {
        return Err(());
    }
    for entry in fs::read_dir(source).map_err(|_| ())? {
        let entry = entry.map_err(|_| ())?;
        let name = entry.file_name();
        if name.to_str().is_none() {
            return Err(());
        }
        let source_path = entry.path();
        let destination_path = destination.join(&name);
        let before = fs::symlink_metadata(&source_path).map_err(|_| ())?;
        if before.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(());
        }
        state.entries = state.entries.checked_add(1).ok_or(())?;
        if state.entries > MAX_IMAGE_ENTRIES {
            return Err(());
        }
        if before.is_dir() {
            fs::create_dir(&destination_path).map_err(|_| ())?;
            copy_directory_contents(&source_path, &destination_path, depth + 1, state)?;
            let after = fs::symlink_metadata(&source_path).map_err(|_| ())?;
            if !after.is_dir() || after.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(());
            }
        } else if before.is_file() {
            state.bytes = state.bytes.checked_add(before.file_size()).ok_or(())?;
            if state.bytes > MAX_IMAGE_BYTES {
                return Err(());
            }
            copy_regular_file(&source_path, &destination_path, before.file_size())?;
        } else {
            return Err(());
        }
    }
    Ok(())
}

fn copy_regular_file(source: &Path, destination: &Path, expected_size: u64) -> Result<(), ()> {
    let mut input = File::open(source).map_err(|_| ())?;
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(|_| ())?;
    let mut copied = 0_u64;
    let mut buffer = [0_u8; MAX_COPY_BUFFER_BYTES];
    loop {
        let read = input.read(&mut buffer).map_err(|_| ())?;
        if read == 0 {
            break;
        }
        output.write_all(&buffer[..read]).map_err(|_| ())?;
        copied = copied
            .checked_add(u64::try_from(read).map_err(|_| ())?)
            .ok_or(())?;
        if copied > expected_size {
            return Err(());
        }
    }
    output.sync_all().map_err(|_| ())?;
    let after = fs::symlink_metadata(source).map_err(|_| ())?;
    if copied != expected_size
        || !after.is_file()
        || after.file_size() != expected_size
        || after.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(());
    }
    Ok(())
}

fn copy_runner(destination: &Path) -> Result<(), ()> {
    let source = std::env::current_exe().map_err(|_| ())?;
    let metadata = fs::symlink_metadata(&source).map_err(|_| ())?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(());
    }
    copy_regular_file(&source, destination, metadata.file_size())
}

fn remove_tree(root: &Path) -> Result<(), ()> {
    fs::remove_dir_all(root).map_err(|_| ())
}

fn utf8(path: &Path) -> Result<String, ()> {
    path.to_str().map(str::to_owned).ok_or(())
}

fn hexadecimal(identifier: &[u8; 16]) -> String {
    identifier
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
