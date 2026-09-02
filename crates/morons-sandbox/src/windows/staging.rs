use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::runner::PreparedRequest;

const MAX_IMAGE_ENTRIES: u64 = 200_000;
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_COPY_BUFFER_BYTES: usize = 64 * 1024;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

pub(super) struct Layout {
    pub(super) root: PathBuf,
    pub(super) image: PathBuf,
    pub(super) runtime: PathBuf,
    pub(super) executable: PathBuf,
}

impl Layout {
    pub(super) fn prepare(request: &PreparedRequest) -> Result<Self, ()> {
        let root = request.scratch_root.join(format!(
            ".morons-windows-{}",
            hexadecimal(&request.operation_id)
        ));
        fs::create_dir(&root).map_err(|_| ())?;
        let image = root.join("image");
        let runtime = root.join("runtime");
        let temporary = runtime.join("tmp");
        let home = runtime.join("home");
        let local = runtime.join("local-app-data");
        let roaming = runtime.join("app-data");
        let public = runtime.join("public");
        let cargo = runtime.join("cargo-home");
        let prepared = (|| {
            for directory in [
                &image, &runtime, &temporary, &home, &local, &roaming, &public, &cargo,
            ] {
                fs::create_dir(directory).map_err(|_| ())?;
            }
            copy_image(&request.image_root, &image)?;
            let relative = request
                .executable
                .strip_prefix(&request.image_root)
                .map_err(|_| ())?;
            let executable = image.join(relative);
            let metadata = fs::symlink_metadata(&executable).map_err(|_| ())?;
            if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(());
            }
            Ok(Self {
                root: root.clone(),
                image,
                runtime,
                executable,
            })
        })();
        if prepared.is_err() {
            let _ = remove_tree(&root);
        }
        prepared
    }

    pub(super) fn cleanup(&self) -> Result<(), ()> {
        remove_tree(&self.root)
    }
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
    let mut copied = 0u64;
    let mut buffer = [0u8; MAX_COPY_BUFFER_BYTES];
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

fn remove_tree(root: &Path) -> Result<(), ()> {
    fs::remove_dir_all(root).map_err(|_| ())
}

fn hexadecimal(identifier: &[u8; 16]) -> String {
    identifier
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
