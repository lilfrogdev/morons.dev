use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

#[cfg(windows)]
use std::io::{self, Seek as _, SeekFrom};

use super::ConnectOrStartError;

#[cfg(windows)]
const CLIENT_EXECUTABLE_NAME: &str = "morons.exe";
#[cfg(not(windows))]
const CLIENT_EXECUTABLE_NAME: &str = "morons";
#[cfg(windows)]
const SERVER_EXECUTABLE_NAME: &str = "morons-server.exe";
#[cfg(not(windows))]
const SERVER_EXECUTABLE_NAME: &str = "morons-server";

pub(super) fn discover_companion_executable() -> Result<PathBuf, ConnectOrStartError> {
    let current = std::env::current_exe().map_err(ConnectOrStartError::CompanionIo)?;
    let current = fs::canonicalize(current).map_err(ConnectOrStartError::CompanionIo)?;
    discover_companion_from(&current)
}

fn discover_companion_from(current: &Path) -> Result<PathBuf, ConnectOrStartError> {
    if current.file_name().and_then(|name| name.to_str()) != Some(CLIENT_EXECUTABLE_NAME) {
        return Err(ConnectOrStartError::CompanionInvalid {
            reason: "the client executable has an unexpected packaged name",
        });
    }
    let parent = current
        .parent()
        .ok_or(ConnectOrStartError::CompanionInvalid {
            reason: "the client executable has no installation directory",
        })?;
    validate_installation_directory(parent)?;
    validate_packaged_file(current, false)?;

    let companion = parent.join(SERVER_EXECUTABLE_NAME);
    validate_packaged_file(&companion, true)?;
    let canonical = fs::canonicalize(&companion).map_err(ConnectOrStartError::CompanionIo)?;
    if canonical.parent() != Some(parent) {
        return Err(ConnectOrStartError::CompanionInvalid {
            reason: "the server companion escapes the installation directory",
        });
    }
    Ok(canonical)
}

fn validate_installation_directory(path: &Path) -> Result<(), ConnectOrStartError> {
    let metadata = fs::symlink_metadata(path).map_err(ConnectOrStartError::CompanionIo)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ConnectOrStartError::CompanionInvalid {
            reason: "the installation directory is not an ordinary directory",
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let effective_user = rustix::process::geteuid().as_raw();
        if (metadata.uid() != 0 && metadata.uid() != effective_user) || metadata.mode() & 0o022 != 0
        {
            return Err(ConnectOrStartError::CompanionInvalid {
                reason: "the installation directory is writable by an untrusted user",
            });
        }
    }

    #[cfg(windows)]
    if !fence_windows::private_directory_is_hardened(path)
        .map_err(|error| ConnectOrStartError::CompanionIo(io::Error::other(error.to_string())))?
    {
        return Err(ConnectOrStartError::CompanionInvalid {
            reason: "the installation directory DACL is not protected",
        });
    }
    Ok(())
}

fn validate_packaged_file(
    path: &Path,
    require_executable: bool,
) -> Result<(), ConnectOrStartError> {
    let metadata = fs::symlink_metadata(path).map_err(ConnectOrStartError::CompanionIo)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ConnectOrStartError::CompanionInvalid {
            reason: "a packaged executable is not an ordinary file",
        });
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let effective_user = rustix::process::geteuid().as_raw();
        if (metadata.uid() != 0 && metadata.uid() != effective_user)
            || metadata.mode() & 0o022 != 0
            || (require_executable && metadata.mode() & 0o111 == 0)
        {
            return Err(ConnectOrStartError::CompanionInvalid {
                reason: "a packaged executable has insecure ownership or permissions",
            });
        }
    }
    let _ = require_executable;
    validate_binary_format(path)
}

fn validate_binary_format(path: &Path) -> Result<(), ConnectOrStartError> {
    let invalid = || ConnectOrStartError::CompanionInvalid {
        reason: "a packaged executable has an unexpected binary format",
    };

    #[cfg(target_os = "linux")]
    {
        let mut header = [0_u8; 20];
        fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|_| invalid())?;
        let expected_machine = if cfg!(target_arch = "x86_64") {
            62
        } else if cfg!(target_arch = "aarch64") {
            183
        } else {
            return Err(invalid());
        };
        if &header[..4] != b"\x7fELF"
            || header[4] != 2
            || header[5] != 1
            || u16::from_le_bytes([header[18], header[19]]) != expected_machine
        {
            return Err(invalid());
        }
    }

    #[cfg(target_os = "macos")]
    {
        let mut header = [0_u8; 8];
        fs::File::open(path)
            .and_then(|mut file| file.read_exact(&mut header))
            .map_err(|_| invalid())?;
        let expected_cpu = if cfg!(target_arch = "x86_64") {
            0x0100_0007
        } else if cfg!(target_arch = "aarch64") {
            0x0100_000c
        } else {
            return Err(invalid());
        };
        if header[..4] != [0xcf, 0xfa, 0xed, 0xfe]
            || u32::from_le_bytes(header[4..8].try_into().map_err(|_| invalid())?) != expected_cpu
        {
            return Err(invalid());
        }
    }

    #[cfg(windows)]
    {
        let mut file = fs::File::open(path).map_err(|_| invalid())?;
        let mut dos_header = [0_u8; 64];
        file.read_exact(&mut dos_header).map_err(|_| invalid())?;
        if &dos_header[..2] != b"MZ" {
            return Err(invalid());
        }
        let pe_offset = u64::from(u32::from_le_bytes(
            dos_header[60..64].try_into().map_err(|_| invalid())?,
        ));
        if !(64..=1024 * 1024).contains(&pe_offset) {
            return Err(invalid());
        }
        file.seek(SeekFrom::Start(pe_offset))
            .map_err(|_| invalid())?;
        let mut pe_header = [0_u8; 6];
        file.read_exact(&mut pe_header).map_err(|_| invalid())?;
        let expected_machine = if cfg!(target_arch = "x86_64") {
            0x8664
        } else if cfg!(target_arch = "aarch64") {
            0xaa64
        } else {
            return Err(invalid());
        };
        if &pe_header[..4] != b"PE\0\0"
            || u16::from_le_bytes([pe_header[4], pe_header[5]]) != expected_machine
        {
            return Err(invalid());
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    return Err(invalid());

    Ok(())
}

pub(super) fn spawn_companion(path: &Path) -> Result<Child, ConnectOrStartError> {
    companion_command(path)
        .spawn()
        .map_err(ConnectOrStartError::CompanionIo)
}

fn companion_command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .env_clear()
        .current_dir(path.parent().expect("validated companion has a parent"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    command
}

pub(super) fn reap_exited_child(child: &mut Option<Child>) -> Result<(), ConnectOrStartError> {
    let Some(process) = child.as_mut() else {
        return Ok(());
    };
    if process
        .try_wait()
        .map_err(ConnectOrStartError::CompanionIo)?
        .is_some()
    {
        *child = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn companion_discovery_rejects_scripts_links_and_writable_installations() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = test_installation("companion-path");
        let client = root.join(CLIENT_EXECUTABLE_NAME);
        let companion = root.join(SERVER_EXECUTABLE_NAME);
        let executable = std::env::current_exe().expect("test executable should be available");
        fs::copy(&executable, &client).expect("test client should be written");
        fs::copy(&executable, &companion).expect("test companion should be written");
        fs::set_permissions(&client, fs::Permissions::from_mode(0o700))
            .expect("test client permissions should be set");
        fs::set_permissions(&companion, fs::Permissions::from_mode(0o700))
            .expect("test companion permissions should be set");
        let client = fs::canonicalize(client).expect("test client should canonicalize");
        let companion = fs::canonicalize(companion).expect("test companion should canonicalize");
        assert_eq!(
            discover_companion_from(&client).expect("secure companion should be discovered"),
            companion
        );

        fs::remove_file(&companion).expect("test companion should be removed");
        fs::write(&companion, b"#!/bin/sh\nexit 0\n").expect("test script should be written");
        fs::set_permissions(&companion, fs::Permissions::from_mode(0o700))
            .expect("test script permissions should be set");
        assert!(matches!(
            discover_companion_from(&client),
            Err(ConnectOrStartError::CompanionInvalid { .. })
        ));
        fs::remove_file(&companion).expect("test script should be removed");
        symlink(&client, &companion).expect("test companion link should be created");
        assert!(matches!(
            discover_companion_from(&client),
            Err(ConnectOrStartError::CompanionInvalid { .. })
        ));
        fs::remove_file(&companion).expect("test companion link should be removed");
        fs::copy(&executable, &companion).expect("test companion should be restored");
        fs::set_permissions(&companion, fs::Permissions::from_mode(0o700))
            .expect("test companion permissions should be restored");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777))
            .expect("test installation permissions should be changed");
        assert!(matches!(
            discover_companion_from(&client),
            Err(ConnectOrStartError::CompanionInvalid { .. })
        ));
        fs::remove_dir_all(root).expect("test installation should be removable");
    }

    #[cfg(windows)]
    #[test]
    fn companion_discovery_accepts_a_protected_windows_package() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test time should be available")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "morons-cli-package-{}-{nonce:x}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("test package should be created");
        fence_windows::harden_private_directory(&root)
            .expect("test package should receive a protected DACL");
        let client = root.join(CLIENT_EXECUTABLE_NAME);
        let companion = root.join(SERVER_EXECUTABLE_NAME);
        let executable = std::env::current_exe().expect("test executable should be available");
        fs::copy(&executable, &client).expect("test client should be written");
        fs::copy(&executable, &companion).expect("test companion should be written");
        let client = fs::canonicalize(client).expect("test client should canonicalize");
        let companion = fs::canonicalize(companion).expect("test companion should canonicalize");
        assert_eq!(
            discover_companion_from(&client).expect("protected companion should be discovered"),
            companion
        );
        fs::remove_dir_all(root).expect("test package should be removable");
    }

    #[test]
    fn companion_command_has_only_reviewed_environment_and_working_directory() {
        let path = Path::new(if cfg!(windows) {
            r"C:\morons\morons-server.exe"
        } else {
            "/opt/morons/morons-server"
        });
        let command = companion_command(path);
        let names = command
            .get_envs()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        #[cfg(unix)]
        assert_eq!(names, vec!["HOME"]);
        #[cfg(windows)]
        assert!(names.is_empty());
        assert_eq!(command.get_current_dir(), path.parent());
        assert_eq!(command.get_program(), path.as_os_str());
    }

    #[cfg(unix)]
    fn test_installation(label: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test time should be available")
            .as_nanos();
        let root = PathBuf::from("/tmp").join(format!(
            "morons-cli-{label}-{}-{nonce:x}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("test installation should be created");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("test installation permissions should be set");
        root
    }
}
