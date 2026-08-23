use std::io;

use interprocess::local_socket::Name;

#[cfg(unix)]
use {
    interprocess::local_socket::{GenericFilePath, prelude::*},
    std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf},
};

#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, prelude::*};

#[cfg(unix)]
const SOCKET_FILE_NAME: &str = "server.sock";

#[cfg(windows)]
const WINDOWS_PIPE_NAME: &str = "morons.dev.server";

/// Returns the fixed cross-platform endpoint used by the local client and server.
pub fn local_socket_name() -> io::Result<Name<'static>> {
    #[cfg(unix)]
    {
        unix_runtime_directory()?
            .join(SOCKET_FILE_NAME)
            .to_fs_name::<GenericFilePath>()
    }

    #[cfg(windows)]
    {
        WINDOWS_PIPE_NAME.to_ns_name::<GenericNamespaced>()
    }

    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "local sockets are unsupported on this platform",
        ))
    }
}

#[cfg(unix)]
fn unix_runtime_directory() -> io::Result<PathBuf> {
    let directory = if let Some(runtime_directory) =
        env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty())
    {
        PathBuf::from(runtime_directory).join("morons.dev")
    } else {
        let home_directory = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;

        PathBuf::from(home_directory).join(".morons").join("run")
    };

    if !directory.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local socket directory must be absolute",
        ));
    }

    fs::create_dir_all(&directory)?;

    if !fs::symlink_metadata(&directory)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "local socket directory is not a directory",
        ));
    }

    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;

    Ok(directory)
}
