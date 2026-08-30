use std::{
    error::Error,
    fmt, fs,
    fs::{File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicBool, Ordering},
};

use interprocess::local_socket::{tokio::Listener, tokio::Stream, tokio::prelude::*};
use serde::{Deserialize, Serialize};

use crate::{
    AUTH_PROTOCOL_VERSION, AUTHENTICATION_KEY_BYTES, AuthenticationKey, HostEpoch, RandomnessError,
    endpoint::{LocalEndpoint, encode_hex, remove_stale_runtime_endpoints},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

const REGISTRATION_SCHEMA_VERSION: u32 = 1;
const AUTHENTICATION_KEY_FILE_NAME: &str = "authentication.key";
const HOST_LOCK_FILE_NAME: &str = "host.lock";
const REGISTRATION_FILE_NAME: &str = "endpoint.json";
const MAX_REGISTRATION_BYTES: u64 = 4096;

#[derive(Debug)]
#[non_exhaustive]
pub enum ControlError {
    Io(io::Error),
    Json(serde_json::Error),
    Randomness(RandomnessError),
    HostAlreadyRunning,
    InvalidState { reason: &'static str },
}

impl fmt::Display for ControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "local control I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "local control JSON is invalid: {error}"),
            Self::Randomness(error) => {
                write!(formatter, "local control randomness failed: {error}")
            }
            Self::HostAlreadyRunning => {
                formatter.write_str("another server owns the local control root")
            }
            Self::InvalidState { reason } => {
                write!(formatter, "local control state is invalid: {reason}")
            }
        }
    }
}

impl Error for ControlError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Randomness(error) => Some(error),
            Self::HostAlreadyRunning | Self::InvalidState { .. } => None,
        }
    }
}

impl From<io::Error> for ControlError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ControlError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<RandomnessError> for ControlError {
    fn from(error: RandomnessError) -> Self {
        Self::Randomness(error)
    }
}

pub struct ServerEndpoint {
    listener: Listener,
    control: ServerControl,
}

impl ServerEndpoint {
    pub fn bind() -> Result<Self, ControlError> {
        Self::bind_with_paths(ControlPaths::discover()?)
    }

    pub async fn accept(&self) -> io::Result<Stream> {
        self.listener.accept().await
    }

    #[must_use]
    pub fn authentication_key(&self) -> &AuthenticationKey {
        &self.control.authentication_key
    }

    #[must_use]
    pub const fn host_epoch(&self) -> &HostEpoch {
        &self.control.host_epoch
    }

    pub fn claim_persistence_root(&self) -> Result<&Path, ControlError> {
        self.control
            .persistence_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ControlError::InvalidState {
                reason: "the server persistence root was already claimed",
            })?;
        Ok(&self.control.paths.root_directory)
    }

    fn bind_with_paths(paths: ControlPaths) -> Result<Self, ControlError> {
        let mut control = ServerControl::acquire(paths)?;
        let listener = control
            .endpoint
            .listener_options()?
            .create_tokio()
            .map_err(ControlError::from)?;
        control.endpoint.secure_bound_endpoint()?;
        control.publish()?;
        Ok(Self { listener, control })
    }
}

pub struct ClientEndpoint {
    authentication_key: AuthenticationKey,
    host_epoch: HostEpoch,
    server_process_id: u32,
    endpoint: LocalEndpoint,
}

impl ClientEndpoint {
    pub fn load() -> Result<Self, ControlError> {
        Self::load_with_paths(ControlPaths::discover()?)
    }

    pub async fn connect(&self) -> io::Result<Stream> {
        Stream::connect(self.endpoint.name()).await
    }

    pub fn verify_connected_server(&self, connection: &Stream) -> io::Result<()> {
        crate::endpoint::verify_connected_server_peer(connection, self.server_process_id)
    }

    #[must_use]
    pub const fn authentication_key(&self) -> &AuthenticationKey {
        &self.authentication_key
    }

    #[must_use]
    pub const fn host_epoch(&self) -> &HostEpoch {
        &self.host_epoch
    }

    fn load_with_paths(paths: ControlPaths) -> Result<Self, ControlError> {
        paths.validate_for_client()?;
        let authentication_key = load_authentication_key(&paths.authentication_key_path())?;
        let registration = read_registration(&paths.registration_path())?;
        let (host_epoch, endpoint) = registration.validate(&paths)?;

        Ok(Self {
            authentication_key,
            host_epoch,
            server_process_id: registration.server_process_id,
            endpoint,
        })
    }
}

struct ServerControl {
    paths: ControlPaths,
    authentication_key: AuthenticationKey,
    host_epoch: HostEpoch,
    endpoint: LocalEndpoint,
    registration: EndpointRegistration,
    _host_lock: File,
    persistence_claimed: AtomicBool,
    published: bool,
}

impl ServerControl {
    fn acquire(paths: ControlPaths) -> Result<Self, ControlError> {
        let control_directory_existed = paths.prepare_for_server()?;
        let host_lock = acquire_host_lock(&paths, control_directory_existed)?;
        let authentication_key = if paths.authentication_key_path().try_exists()? {
            load_authentication_key(&paths.authentication_key_path())?
        } else if control_directory_existed {
            return Err(ControlError::InvalidState {
                reason: "an existing control root is missing its authentication key",
            });
        } else {
            create_authentication_key(&paths.authentication_key_path())?
        };

        remove_stale_registration(&paths)?;
        remove_stale_registration_temporary_files(&paths)?;
        remove_stale_runtime_endpoints(&paths.runtime_directory)?;

        let host_epoch = HostEpoch::generate()?;
        let endpoint = LocalEndpoint::new(&paths.runtime_directory, &host_epoch)?;
        let registration = EndpointRegistration {
            registration_schema_version: REGISTRATION_SCHEMA_VERSION,
            authentication_protocol_version: AUTH_PROTOCOL_VERSION,
            host_epoch: encode_hex(host_epoch.as_bytes()),
            endpoint: endpoint.identifier().to_owned(),
            server_process_id: process::id(),
        };

        Ok(Self {
            paths,
            authentication_key,
            host_epoch,
            endpoint,
            registration,
            _host_lock: host_lock,
            persistence_claimed: AtomicBool::new(false),
            published: false,
        })
    }

    fn publish(&mut self) -> Result<(), ControlError> {
        if self.published {
            return Err(ControlError::InvalidState {
                reason: "the endpoint registration is already published",
            });
        }
        if self.paths.registration_path().try_exists()? {
            return Err(ControlError::InvalidState {
                reason: "an endpoint registration appeared while the host lock was held",
            });
        }

        write_registration(&self.paths, &self.registration)?;
        self.published = true;
        Ok(())
    }
}

impl Drop for ServerControl {
    fn drop(&mut self) {
        if self.published
            && let Ok(registration) = read_registration(&self.paths.registration_path())
            && registration == self.registration
        {
            let _ = fs::remove_file(self.paths.registration_path());
            let _ = sync_directory(&self.paths.control_directory);
        }
        let _ = self.endpoint.remove_bound_endpoint();
    }
}

#[derive(Clone)]
struct ControlPaths {
    root_directory: PathBuf,
    control_directory: PathBuf,
    runtime_directory: PathBuf,
}

impl ControlPaths {
    fn discover() -> Result<Self, ControlError> {
        #[cfg(unix)]
        {
            let home = std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .ok_or(ControlError::InvalidState {
                    reason: "HOME is not set",
                })?;
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err(ControlError::InvalidState {
                    reason: "HOME must be absolute",
                });
            }
            validate_unix_home_directory(&home)?;
            Ok(Self::from_root(home.join(".morons")))
        }

        #[cfg(windows)]
        {
            let local_app_data = fence_windows::local_app_data()
                .map_err(|error| io::Error::other(error.to_string()))?;
            Ok(Self::from_root(local_app_data.join("morons.dev")))
        }

        #[cfg(not(any(unix, windows)))]
        {
            Err(ControlError::InvalidState {
                reason: "local control is unsupported on this platform",
            })
        }
    }

    fn from_root(root_directory: PathBuf) -> Self {
        Self {
            control_directory: root_directory.join("control"),
            runtime_directory: root_directory.join("run"),
            root_directory,
        }
    }

    fn prepare_for_server(&self) -> Result<bool, ControlError> {
        let control_directory_existed = self.control_directory.try_exists()?;
        ensure_private_directory(&self.root_directory)?;
        ensure_private_directory(&self.control_directory)?;

        #[cfg(unix)]
        ensure_private_directory(&self.runtime_directory)?;

        Ok(control_directory_existed)
    }

    fn validate_for_client(&self) -> Result<(), ControlError> {
        validate_private_directory(&self.root_directory)?;
        validate_private_directory(&self.control_directory)?;

        #[cfg(unix)]
        validate_private_directory(&self.runtime_directory)?;

        Ok(())
    }

    fn authentication_key_path(&self) -> PathBuf {
        self.control_directory.join(AUTHENTICATION_KEY_FILE_NAME)
    }

    fn host_lock_path(&self) -> PathBuf {
        self.control_directory.join(HOST_LOCK_FILE_NAME)
    }

    fn registration_path(&self) -> PathBuf {
        self.control_directory.join(REGISTRATION_FILE_NAME)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointRegistration {
    registration_schema_version: u32,
    authentication_protocol_version: u32,
    host_epoch: String,
    endpoint: String,
    server_process_id: u32,
}

impl EndpointRegistration {
    fn validate(&self, paths: &ControlPaths) -> Result<(HostEpoch, LocalEndpoint), ControlError> {
        if self.registration_schema_version != REGISTRATION_SCHEMA_VERSION {
            return Err(ControlError::InvalidState {
                reason: "the endpoint registration schema version is unsupported",
            });
        }
        if self.authentication_protocol_version != AUTH_PROTOCOL_VERSION {
            return Err(ControlError::InvalidState {
                reason: "the authentication protocol version is unsupported",
            });
        }
        if self.server_process_id == 0 {
            return Err(ControlError::InvalidState {
                reason: "the registered server process ID is invalid",
            });
        }

        let host_epoch = decode_host_epoch(&self.host_epoch)?;
        let endpoint =
            LocalEndpoint::from_identifier(&paths.runtime_directory, &host_epoch, &self.endpoint)?;
        Ok((host_epoch, endpoint))
    }
}

fn acquire_host_lock(
    paths: &ControlPaths,
    control_directory_existed: bool,
) -> Result<File, ControlError> {
    let path = paths.host_lock_path();
    let lock_existed = path.try_exists()?;
    if control_directory_existed && !lock_existed {
        return Err(ControlError::InvalidState {
            reason: "an existing control root is missing its stable host lock",
        });
    }

    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if lock_existed {
        options.create(false);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    options.mode(0o600);

    let lock = options.open(&path)?;
    validate_private_file(&path, None)?;
    if !lock_existed {
        lock.sync_all()?;
        sync_directory(&paths.control_directory)?;
    }
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(TryLockError::WouldBlock) => Err(ControlError::HostAlreadyRunning),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

fn create_authentication_key(path: &Path) -> Result<AuthenticationKey, ControlError> {
    let key = AuthenticationKey::generate()?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    let result = (|| -> Result<(), ControlError> {
        let mut file = options.open(path)?;
        file.write_all(key.as_bytes())?;
        file.sync_all()?;
        validate_private_file(path, Some(AUTHENTICATION_KEY_BYTES as u64))?;
        sync_directory(path.parent().ok_or(ControlError::InvalidState {
            reason: "the authentication key has no parent directory",
        })?)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result.map(|()| key)
}

fn load_authentication_key(path: &Path) -> Result<AuthenticationKey, ControlError> {
    validate_private_file(path, Some(AUTHENTICATION_KEY_BYTES as u64))?;
    let mut file = File::open(path)?;
    let mut bytes = [0_u8; AUTHENTICATION_KEY_BYTES];
    file.read_exact(&mut bytes)?;
    let mut trailing = [0_u8; 1];
    if file.read(&mut trailing)? != 0 {
        return Err(ControlError::InvalidState {
            reason: "the authentication key has trailing bytes",
        });
    }
    Ok(AuthenticationKey::from_bytes(bytes))
}

fn write_registration(
    paths: &ControlPaths,
    registration: &EndpointRegistration,
) -> Result<(), ControlError> {
    let payload = serde_json::to_vec(registration)?;
    if payload.len() > MAX_REGISTRATION_BYTES as usize {
        return Err(ControlError::InvalidState {
            reason: "the endpoint registration exceeds its size limit",
        });
    }

    let temporary_path = paths.control_directory.join(format!(
        ".{REGISTRATION_FILE_NAME}.{}.tmp",
        registration.host_epoch
    ));
    let final_path = paths.registration_path();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    let result = (|| -> Result<(), ControlError> {
        let mut file = options.open(&temporary_path)?;
        file.write_all(&payload)?;
        file.sync_all()?;
        validate_private_file(&temporary_path, Some(payload.len() as u64))?;
        fs::rename(&temporary_path, &final_path)?;
        sync_directory(&paths.control_directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn read_registration(path: &Path) -> Result<EndpointRegistration, ControlError> {
    validate_private_file(path, None)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.len() > MAX_REGISTRATION_BYTES {
        return Err(ControlError::InvalidState {
            reason: "the endpoint registration exceeds its size limit",
        });
    }

    let mut payload = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(MAX_REGISTRATION_BYTES + 1)
        .read_to_end(&mut payload)?;
    if payload.len() > MAX_REGISTRATION_BYTES as usize {
        return Err(ControlError::InvalidState {
            reason: "the endpoint registration changed beyond its size limit",
        });
    }

    Ok(serde_json::from_slice(&payload)?)
}

fn remove_stale_registration(paths: &ControlPaths) -> Result<(), ControlError> {
    let path = paths.registration_path();
    if !path.try_exists()? {
        return Ok(());
    }

    let registration = read_registration(&path)?;
    let (host_epoch, endpoint) = registration.validate(paths)?;
    let _ = host_epoch;
    endpoint.remove_bound_endpoint()?;
    fs::remove_file(path)?;
    sync_directory(&paths.control_directory)?;
    Ok(())
}

fn remove_stale_registration_temporary_files(paths: &ControlPaths) -> Result<(), ControlError> {
    let prefix = format!(".{REGISTRATION_FILE_NAME}.");
    for entry in fs::read_dir(&paths.control_directory)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(encoded_epoch) = file_name
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(".tmp"))
        else {
            continue;
        };
        if decode_host_epoch(encoded_epoch).is_err() {
            continue;
        }

        validate_private_file(&entry.path(), None)?;
        fs::remove_file(entry.path())?;
    }
    sync_directory(&paths.control_directory)?;
    Ok(())
}

fn decode_host_epoch(encoded: &str) -> Result<HostEpoch, ControlError> {
    if encoded.len() != crate::HOST_EPOCH_BYTES * 2 {
        return Err(ControlError::InvalidState {
            reason: "the registered Host Epoch has an invalid length",
        });
    }

    let bytes = encoded.as_bytes();
    let mut decoded = [0_u8; crate::HOST_EPOCH_BYTES];
    for (index, output) in decoded.iter_mut().enumerate() {
        let high = decode_hex_digit(bytes[index * 2]).ok_or(ControlError::InvalidState {
            reason: "the registered Host Epoch is not lowercase hexadecimal",
        })?;
        let low = decode_hex_digit(bytes[index * 2 + 1]).ok_or(ControlError::InvalidState {
            reason: "the registered Host Epoch is not lowercase hexadecimal",
        })?;
        *output = high << 4 | low;
    }
    Ok(HostEpoch::from_bytes(decoded))
}

fn decode_hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(unix)]
fn validate_unix_home_directory(path: &Path) -> Result<(), ControlError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(ControlError::InvalidState {
            reason: "HOME is not an owner-controlled directory",
        });
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), ControlError> {
    let created = !path.try_exists()?;
    if created {
        #[cfg(unix)]
        let builder = {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        builder.create(path)?;
    }

    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(ControlError::InvalidState {
                reason: "a control path is not an ordinary directory",
            });
        }
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(ControlError::InvalidState {
                reason: "a control directory has an unexpected owner",
            });
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }

    #[cfg(windows)]
    {
        let is_hardened = fence_windows::private_directory_is_hardened(path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if !is_hardened {
            fence_windows::harden_private_directory(path)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
    }

    validate_private_directory(path)?;
    if created && let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), ControlError> {
    #[cfg(unix)]
    {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(ControlError::InvalidState {
                reason: "a control directory is not owner-only",
            });
        }
    }

    #[cfg(windows)]
    {
        let is_hardened = fence_windows::private_directory_is_hardened(path)
            .map_err(|error| io::Error::other(error.to_string()))?;
        if !is_hardened {
            return Err(ControlError::InvalidState {
                reason: "a control directory DACL is not owner-only",
            });
        }
    }

    Ok(())
}

fn validate_private_file(path: &Path, expected_bytes: Option<u64>) -> Result<(), ControlError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ControlError::InvalidState {
            reason: "a control file is not an ordinary file",
        });
    }
    if expected_bytes.is_some_and(|expected| metadata.len() != expected) {
        return Err(ControlError::InvalidState {
            reason: "a control file has an unexpected length",
        });
    }

    #[cfg(unix)]
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o777 != 0o600 {
        return Err(ControlError::InvalidState {
            reason: "a control file is not owner-only",
        });
    }

    #[cfg(windows)]
    validate_private_directory(path.parent().ok_or(ControlError::InvalidState {
        reason: "a control file has no parent directory",
    })?)?;

    Ok(())
}

fn sync_directory(path: &Path) -> io::Result<()> {
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

#[cfg(test)]
mod tests;
