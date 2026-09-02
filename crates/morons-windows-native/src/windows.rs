mod acl;
mod launch;
mod profile;

use std::fmt;

pub use launch::{CommandCompletion, CommandLaunch, CommandLimits, CommandProcess};
pub use profile::OperationProfile;

pub struct OperationPaths<'a> {
    pub candidate: &'a std::path::Path,
    pub runtime: &'a std::path::Path,
    pub image: &'a std::path::Path,
}

#[derive(Clone, Copy)]
enum Access {
    ReadExecute,
    ReadWriteExecute,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct NativeError {
    stage: &'static str,
    code: u32,
}

impl NativeError {
    fn last(stage: &'static str) -> Self {
        let code = std::io::Error::last_os_error()
            .raw_os_error()
            .map(|code| code as u32)
            .unwrap_or(0);
        Self { stage, code }
    }

    fn code(stage: &'static str, code: u32) -> Self {
        Self { stage, code }
    }

    pub fn stage(self) -> &'static str {
        self.stage
    }

    pub fn os_code(self) -> u32 {
        self.code
    }
}

impl fmt::Debug for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeError")
            .field("stage", &self.stage)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Windows sandbox stage {} failed", self.stage)
    }
}

impl std::error::Error for NativeError {}

fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>, NativeError> {
    use std::os::windows::ffi::OsStrExt;

    let units = value.encode_wide().collect::<Vec<_>>();
    if units.is_empty() || units.contains(&0) {
        return Err(NativeError::code("input", 0));
    }
    Ok(units.into_iter().chain(std::iter::once(0)).collect())
}
