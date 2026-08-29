use std::{io, path::Path};

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

    pub(crate) fn listener_options(&self) -> io::Result<ListenerOptions<'static>> {
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
