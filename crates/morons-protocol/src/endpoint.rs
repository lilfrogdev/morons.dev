use std::{io, path::Path};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use interprocess::local_socket::{ListenerOptions, Name, tokio::Stream};

use crate::HostEpoch;

#[cfg(unix)]
use {
    interprocess::local_socket::{GenericFilePath, prelude::*},
    std::{
        fs,
        os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
        path::PathBuf,
    },
};

#[cfg(windows)]
use {
    interprocess::{
        local_socket::{GenericNamespaced, prelude::*},
        os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
    },
    widestring::U16CString,
};

#[cfg(windows)]
const OWNER_ONLY_PIPE_SDDL: &str = "D:P(A;;GA;;;OW)";

/// Closed, redacted endpoint-local reason for a proven native pathname overflow.
/// It deliberately carries no path, epoch, key, or source-error detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativePathCapacityOverflow;

/// Reviewed inclusive `sun_path` capacities: 104 bytes on macOS, 108 on Linux.
#[cfg(unix)]
fn native_pathname_capacity() -> Option<usize> {
    if cfg!(target_os = "macos") {
        Some(104)
    } else if cfg!(target_os = "linux") {
        Some(108)
    } else {
        None
    }
}

pub(crate) struct LocalEndpoint {
    name: Name<'static>,
    identifier: String,
    #[cfg(unix)]
    path: PathBuf,
}

impl LocalEndpoint {
    pub(crate) fn new(runtime_directory: &Path, host_epoch: &HostEpoch) -> io::Result<Self> {
        let identifier = endpoint_identifier(host_epoch);

        #[cfg(unix)]
        {
            let path = runtime_directory.join(&identifier);
            let name = path.clone().to_fs_name::<GenericFilePath>()?;
            Ok(Self {
                name,
                identifier,
                path,
            })
        }

        #[cfg(windows)]
        {
            let _ = runtime_directory;
            let name = identifier.clone().to_ns_name::<GenericNamespaced>()?;
            Ok(Self { name, identifier })
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = runtime_directory;
            let _ = identifier;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local sockets are unsupported on this platform",
            ))
        }
    }

    pub(crate) fn from_identifier(
        runtime_directory: &Path,
        host_epoch: &HostEpoch,
        identifier: &str,
    ) -> io::Result<Self> {
        if identifier != endpoint_identifier(host_epoch) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "registered endpoint does not match its Host Epoch",
            ));
        }

        Self::new(runtime_directory, host_epoch)
    }

    pub(crate) fn name(&self) -> Name<'static> {
        self.name.clone()
    }

    pub(crate) fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Side-effect-free native-byte capacity check on the actually generated Unix
    /// pathname. success only means no proven length overflow; other platforms keep
    /// their existing transport behavior.
    pub(crate) fn check_native_path_capacity(&self) -> Result<(), NativePathCapacityOverflow> {
        #[cfg(unix)]
        {
            let Some(capacity) = native_pathname_capacity() else {
                return Ok(());
            };
            let bytes = self.path.as_os_str().as_bytes();
            if bytes.contains(&0) {
                // NUL input stays with converter binding; never length-labeled.
                return Ok(());
            }
            if bytes.len() > capacity {
                return Err(NativePathCapacityOverflow);
            }
        }

        Ok(())
    }

    pub(crate) fn listener_options(&self) -> io::Result<ListenerOptions<'static>> {
        self.check_native_path_capacity().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "local socket pathname exceeds this platform's capacity",
            )
        })?;

        let options = ListenerOptions::new()
            .name(self.name())
            .reclaim_name(false)
            .try_overwrite(false);

        #[cfg(windows)]
        {
            let sddl = U16CString::from_str(OWNER_ONLY_PIPE_SDDL)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            let descriptor = SecurityDescriptor::deserialize(sddl.as_ucstr())?;
            Ok(options.security_descriptor(descriptor))
        }

        #[cfg(not(windows))]
        {
            Ok(options)
        }
    }

    pub(crate) fn secure_bound_endpoint(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let metadata = fs::symlink_metadata(&self.path)?;
            if !metadata.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "bound local endpoint is not a Unix socket",
                ));
            }
            if metadata.uid() != rustix::process::geteuid().as_raw() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "bound local endpoint has an unexpected owner",
                ));
            }

            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
            let secured = fs::symlink_metadata(&self.path)?;
            if !secured.file_type().is_socket()
                || secured.uid() != rustix::process::geteuid().as_raw()
                || secured.mode() & 0o777 != 0o600
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "bound local endpoint permissions could not be verified",
                ));
            }
        }

        Ok(())
    }

    pub(crate) fn remove_bound_endpoint(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let metadata = match fs::symlink_metadata(&self.path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
            if !metadata.file_type().is_socket()
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "refusing to remove an unexpected local endpoint",
                ));
            }
            fs::remove_file(&self.path)?;
        }

        Ok(())
    }
}

pub(crate) fn remove_stale_runtime_endpoints(runtime_directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        for entry in fs::read_dir(runtime_directory)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if !is_unix_endpoint_identifier(file_name) {
                continue;
            }

            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.file_type().is_socket()
                || metadata.uid() != rustix::process::geteuid().as_raw()
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "refusing to remove an unexpected stale local endpoint",
                ));
            }
            fs::remove_file(entry.path())?;
        }
    }

    #[cfg(not(unix))]
    {
        let _ = runtime_directory;
    }

    Ok(())
}

pub fn authorize_accepted_peer(connection: &Stream) -> io::Result<()> {
    #[cfg(unix)]
    {
        let peer_user = connection
            .peer_creds()?
            .euid()
            .ok_or_else(|| io::Error::other("accepted peer effective user ID is unavailable"))?;
        if peer_user != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "accepted peer effective user ID does not match the server",
            ));
        }
    }

    #[cfg(windows)]
    {
        let _ = connection;
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = connection;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local peer authorization is unsupported on this platform",
        ));
    }

    Ok(())
}

pub fn verify_connected_server_peer(
    connection: &Stream,
    registered_process_id: u32,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        let _ = registered_process_id;
        let peer_user = connection
            .peer_creds()?
            .euid()
            .ok_or_else(|| io::Error::other("server effective user ID is unavailable"))?;
        if peer_user != rustix::process::geteuid().as_raw() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "server effective user ID does not match the client",
            ));
        }
    }

    #[cfg(windows)]
    {
        let peer_process_id = connection
            .peer_creds()?
            .pid()
            .ok_or_else(|| io::Error::other("server process ID is unavailable"))?;
        if peer_process_id != registered_process_id {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "server process ID does not match the endpoint registration",
            ));
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = connection;
        let _ = registered_process_id;
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local peer verification is unsupported on this platform",
        ));
    }

    Ok(())
}

#[cfg(unix)]
fn is_unix_endpoint_identifier(identifier: &str) -> bool {
    const PREFIX: &str = "server-";
    const SUFFIX: &str = ".sock";

    let Some(encoded_epoch) = identifier
        .strip_prefix(PREFIX)
        .and_then(|value| value.strip_suffix(SUFFIX))
    else {
        return false;
    };
    encoded_epoch.len() == crate::HOST_EPOCH_BYTES * 2
        && encoded_epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn endpoint_identifier(host_epoch: &HostEpoch) -> String {
    let encoded_epoch = encode_hex(host_epoch.as_bytes());

    #[cfg(unix)]
    {
        format!("server-{encoded_epoch}.sock")
    }

    #[cfg(windows)]
    {
        format!("morons.dev.server.{encoded_epoch}")
    }

    #[cfg(not(any(unix, windows)))]
    {
        encoded_epoch
    }
}

pub(crate) fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    #[cfg(target_os = "macos")]
    const EXPECTED_CAPACITY: usize = 104;
    #[cfg(target_os = "linux")]
    const EXPECTED_CAPACITY: usize = 108;

    fn synthetic_dir(total_path_len: usize, epoch: &HostEpoch) -> PathBuf {
        let identifier = endpoint_identifier(epoch);
        assert!(total_path_len >= identifier.len() + 3);
        let directory_len = total_path_len - identifier.len() - 1;
        let mut bytes = vec![b'a'; directory_len];
        bytes[0] = b'/';
        let directory = PathBuf::from(OsStr::from_bytes(&bytes));
        assert_eq!(
            directory.join(&identifier).as_os_str().as_bytes().len(),
            total_path_len
        );
        directory
    }

    fn endpoint_with(total_path_len: usize, epoch_bytes: [u8; 16]) -> LocalEndpoint {
        let epoch = HostEpoch::from_bytes(epoch_bytes);
        let directory = synthetic_dir(total_path_len, &epoch);
        let endpoint = LocalEndpoint::new(&directory, &epoch).unwrap();
        assert_eq!(endpoint.path, directory.join(endpoint_identifier(&epoch)));
        assert_eq!(endpoint.path.parent(), Some(directory.as_path()));
        assert_eq!(
            endpoint.path.file_name(),
            Some(OsStr::new(endpoint.identifier()))
        );
        assert_eq!(endpoint.path.as_os_str().as_bytes().len(), total_path_len);
        endpoint
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn assert_capacity(endpoint: &LocalEndpoint, total: usize) {
        assert_eq!(endpoint.path.as_os_str().as_bytes().len(), total);
        if total <= EXPECTED_CAPACITY {
            assert_eq!(endpoint.check_native_path_capacity(), Ok(()));
            assert!(endpoint.listener_options().is_ok());
        } else {
            assert_eq!(
                endpoint.check_native_path_capacity(),
                Err(NativePathCapacityOverflow)
            );
            assert_eq!(
                endpoint.listener_options().unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn epoch_identifiers_are_unchanged() {
        for (bytes, expected) in [
            ([0; 16], "server-00000000000000000000000000000000.sock"),
            ([0xff; 16], "server-ffffffffffffffffffffffffffffffff.sock"),
        ] {
            let epoch = HostEpoch::from_bytes(bytes);
            assert_eq!(endpoint_identifier(&epoch), expected);
            assert_eq!(expected.len(), 44);
            assert_eq!(endpoint_with(80, bytes).identifier(), expected);
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn capacity_boundary_and_identifiers() {
        assert_eq!(native_pathname_capacity(), Some(EXPECTED_CAPACITY));
        for bytes in [[0; 16], [0xff; 16]] {
            for total in [
                EXPECTED_CAPACITY - 1,
                EXPECTED_CAPACITY,
                EXPECTED_CAPACITY + 1,
            ] {
                assert_capacity(&endpoint_with(total, bytes), total);
            }
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn multibyte_paths_count_native_bytes() {
        let epoch = HostEpoch::from_bytes([0x42; 16]);
        for total in [EXPECTED_CAPACITY, EXPECTED_CAPACITY + 1] {
            let directory_len = total - endpoint_identifier(&epoch).len() - 1;
            let directory = format!("/é{}", "a".repeat(directory_len - 3));
            let endpoint = LocalEndpoint::new(Path::new(&directory), &epoch).unwrap();
            assert_eq!(
                endpoint.path,
                Path::new(&directory).join(endpoint.identifier())
            );
            let text = endpoint.path.to_str().unwrap();
            assert!(text.chars().count() <= EXPECTED_CAPACITY);
            assert!(text.chars().count() < text.len());
            assert_capacity(&endpoint, total);
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn non_utf8_paths_count_native_bytes_not_lossy_bytes() {
        let epoch = HostEpoch::from_bytes([0x42; 16]);
        for total in [EXPECTED_CAPACITY, EXPECTED_CAPACITY + 1] {
            let directory_len = total - endpoint_identifier(&epoch).len() - 1;
            let mut bytes = vec![b'a'; directory_len];
            bytes[0] = b'/';
            bytes[1] = 0xff;
            let directory = Path::new(OsStr::from_bytes(&bytes));
            let endpoint = LocalEndpoint::new(directory, &epoch).unwrap();
            assert_eq!(endpoint.path, directory.join(endpoint.identifier()));
            assert!(endpoint.path.to_str().is_none());
            assert!(endpoint.path.to_string_lossy().len() > total);
            assert_capacity(&endpoint, total);
        }
    }

    #[test]
    fn interior_nul_is_never_length_labeled() {
        let epoch = HostEpoch::from_bytes([0x11; 16]);
        for directory in [String::from("/a\0b"), format!("/{}\0b", "a".repeat(120))] {
            let error = match LocalEndpoint::new(Path::new(&directory), &epoch) {
                Err(error) => error,
                Ok(_) => panic!("interior NUL must be rejected"),
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn long_path_constructs_but_listener_rejects_with_fixed_redacted_error() {
        let first = endpoint_with(130, [0; 16]);
        let second = endpoint_with(180, [0xff; 16]);
        assert_capacity(&first, 130);
        assert_capacity(&second, 180);
        let first_error = first.listener_options().unwrap_err();
        let second_error = second.listener_options().unwrap_err();
        assert_eq!(first_error.to_string(), second_error.to_string());
        assert_eq!(format!("{first_error:?}"), format!("{second_error:?}"));
        for text in [first_error.to_string(), format!("{first_error:?}")] {
            assert!(!text.is_empty());
            assert!(!text.contains("server-"));
            assert!(!text.contains(".sock"));
            assert!(!text.contains('/'));
            assert!(!text.contains(&"0".repeat(32)));
            assert!(!text.contains(&"f".repeat(32)));
        }
    }

    #[test]
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn other_unix_targets_have_no_reviewed_capacity_limit() {
        assert_eq!(native_pathname_capacity(), None);
        for total in [103, 104, 105, 107, 108, 109, 130] {
            let endpoint = endpoint_with(total, [0; 16]);
            assert_eq!(endpoint.check_native_path_capacity(), Ok(()));
            assert!(endpoint.listener_options().is_ok());
        }
    }
}
