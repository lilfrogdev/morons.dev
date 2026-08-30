use std::{
    error::Error,
    ffi::OsStr,
    fmt, fs,
    fs::{File, OpenOptions},
    io::{self},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

const DATA_DIRECTORY_NAME: &str = "data";
const WORKSPACE_DIRECTORY_NAME: &str = "workspaces";
const DATABASE_FILE_NAME: &str = "sessions.sqlite3";
const DATABASE_INITIALIZATION_PREFIX: &str = ".sessions.sqlite3.initializing-";
const DATABASE_JOURNAL_SUFFIX: &str = "-journal";
const IDENTIFIER_BYTES: usize = 16;
const HEX_IDENTIFIER_BYTES: usize = IDENTIFIER_BYTES * 2;

#[derive(Debug)]
pub(crate) enum PathError {
    Io(io::Error),
    InvalidState { reason: &'static str },
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(
                formatter,
                "persistence filesystem operation failed: {error}"
            ),
            Self::InvalidState { reason } => {
                write!(
                    formatter,
                    "persistence filesystem state is invalid: {reason}"
                )
            }
        }
    }
}

impl Error for PathError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::InvalidState { .. } => None,
        }
    }
}

impl From<io::Error> for PathError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub(crate) struct StoragePaths {
    data_directory: PathBuf,
    pub(super) workspace_directory: PathBuf,
    database_path: PathBuf,
}

impl StoragePaths {
    pub(crate) fn prepare(application_root: &Path) -> Result<Self, PathError> {
        validate_private_directory(application_root)?;

        let data_directory = application_root.join(DATA_DIRECTORY_NAME);
        let workspace_directory = application_root.join(WORKSPACE_DIRECTORY_NAME);
        ensure_private_directory(&data_directory)?;
        ensure_private_directory(&workspace_directory)?;

        let paths = Self {
            database_path: data_directory.join(DATABASE_FILE_NAME),
            data_directory,
            workspace_directory,
        };
        paths.cleanup_stale_database_initialization_files()?;
        Ok(paths)
    }

    pub(crate) fn database_path(&self) -> &Path {
        &self.database_path
    }

    pub(crate) fn database_exists(&self) -> Result<bool, PathError> {
        path_entry_exists(&self.database_path).map_err(PathError::from)
    }

    pub(crate) fn create_database_initialization_file(
        &self,
        nonce: &[u8; IDENTIFIER_BYTES],
    ) -> Result<(PathBuf, File), PathError> {
        let path = self.data_directory.join(format!(
            "{DATABASE_INITIALIZATION_PREFIX}{}",
            encode_hex(nonce)
        ));
        let file = create_private_file(&path)?;
        Ok((path, file))
    }

    pub(crate) fn install_database(&self, initialization_path: &Path) -> Result<(), PathError> {
        if path_entry_exists(&self.database_path)? {
            return Err(PathError::InvalidState {
                reason: "the authoritative database appeared during initialization",
            });
        }
        validate_private_file(initialization_path, None)?;
        fs::rename(initialization_path, &self.database_path)?;
        sync_directory(&self.data_directory)?;
        validate_private_file(&self.database_path, None)
    }

    pub(crate) fn remove_initialization_file(&self, path: &Path) -> Result<(), PathError> {
        if path.parent() != Some(self.data_directory.as_path())
            || !is_database_initialization_name(path.file_name())
        {
            return Err(PathError::InvalidState {
                reason: "database initialization cleanup escaped its expected path",
            });
        }
        if path_entry_exists(path)? {
            validate_private_file(path, None)?;
            fs::remove_file(path)?;
            sync_directory(&self.data_directory)?;
        }
        Ok(())
    }

    pub(crate) fn validate_database_file(&self, maximum_bytes: u64) -> Result<(), PathError> {
        validate_private_file(&self.database_path, Some(maximum_bytes))?;
        let journal_path = database_sidecar_path(&self.database_path, "-journal");
        if path_entry_exists(&journal_path)? {
            validate_private_file(&journal_path, Some(maximum_bytes))?;
        }
        for suffix in ["-wal", "-shm"] {
            if path_entry_exists(&database_sidecar_path(&self.database_path, suffix))? {
                return Err(PathError::InvalidState {
                    reason: "an unsupported SQLite sidecar exists beside the database",
                });
            }
        }
        Ok(())
    }

    pub(crate) fn validate_database_file_at(
        &self,
        path: &Path,
        maximum_bytes: u64,
    ) -> Result<(), PathError> {
        if path.parent() != Some(self.data_directory.as_path())
            || !is_database_initialization_name(path.file_name())
        {
            return Err(PathError::InvalidState {
                reason: "database validation escaped its expected path",
            });
        }
        validate_private_file(path, Some(maximum_bytes))
    }

    fn cleanup_stale_database_initialization_files(&self) -> Result<(), PathError> {
        let mut removed = false;
        for entry in fs::read_dir(&self.data_directory)? {
            let entry = entry?;
            if !is_database_initialization_name(Some(&entry.file_name())) {
                continue;
            }
            validate_private_file(&entry.path(), None)?;
            fs::remove_file(entry.path())?;
            removed = true;
        }
        if removed {
            sync_directory(&self.data_directory)?;
        }
        Ok(())
    }
}

pub(super) fn ensure_private_directory(path: &Path) -> Result<bool, PathError> {
    let created = if path_entry_exists(path)? {
        false
    } else {
        #[cfg(unix)]
        let result = {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(path)
        };
        #[cfg(not(unix))]
        let result = fs::create_dir(path);

        match result {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(error) => return Err(error.into()),
        }
    };

    #[cfg(windows)]
    if created {
        fence_windows::harden_private_directory(path)
            .map_err(|error| io::Error::other(error.to_string()))?;
    }

    validate_private_directory(path)?;
    if created && let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(created)
}

pub(super) fn validate_private_directory(path: &Path) -> Result<(), PathError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(PathError::InvalidState {
            reason: "a persistence directory is not an ordinary directory",
        });
    }

    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
        return Err(PathError::InvalidState {
            reason: "a persistence directory is not owner-only",
        });
    }

    #[cfg(windows)]
    if !fence_windows::private_directory_is_hardened(path)
        .map_err(|error| io::Error::other(error.to_string()))?
    {
        return Err(PathError::InvalidState {
            reason: "a persistence directory DACL is not owner-only",
        });
    }

    Ok(())
}

pub(super) fn create_private_file(path: &Path) -> Result<File, PathError> {
    #[cfg(unix)]
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;

    #[cfg(not(unix))]
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)?;

    if let Err(error) = validate_private_file(path, None) {
        drop(file);
        fs::remove_file(path)?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        return Err(error);
    }
    Ok(file)
}

pub(super) fn validate_private_file(
    path: &Path,
    maximum_bytes: Option<u64>,
) -> Result<(), PathError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(PathError::InvalidState {
            reason: "a persistence file is not an ordinary file",
        });
    }
    if maximum_bytes.is_some_and(|maximum| metadata.len() > maximum) {
        return Err(PathError::InvalidState {
            reason: "a persistence file exceeds its size limit",
        });
    }

    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o600 {
        return Err(PathError::InvalidState {
            reason: "a persistence file is not owner-only",
        });
    }

    #[cfg(windows)]
    validate_private_directory(path.parent().ok_or(PathError::InvalidState {
        reason: "a persistence file has no parent directory",
    })?)?;

    Ok(())
}

fn is_database_initialization_name(file_name: Option<&OsStr>) -> bool {
    let Some(file_name) = file_name.and_then(OsStr::to_str) else {
        return false;
    };
    let Some(suffix) = file_name.strip_prefix(DATABASE_INITIALIZATION_PREFIX) else {
        return false;
    };
    let identifier = suffix
        .strip_suffix(DATABASE_JOURNAL_SUFFIX)
        .unwrap_or(suffix);
    identifier.len() == HEX_IDENTIFIER_BYTES
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn database_sidecar_path(database_path: &Path, suffix: &str) -> PathBuf {
    let mut path = database_path.as_os_str().to_owned();
    path.push(suffix);
    PathBuf::from(path)
}

pub(super) fn encode_hex(bytes: &[u8; IDENTIFIER_BYTES]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(HEX_IDENTIFIER_BYTES);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }

    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
