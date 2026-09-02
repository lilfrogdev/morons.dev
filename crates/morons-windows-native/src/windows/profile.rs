use std::ffi::{OsStr, c_void};

use windows_sys::Win32::{
    Foundation::ERROR_ALREADY_EXISTS,
    Security::{
        FreeSid,
        Isolation::{
            CreateAppContainerProfile, DeleteAppContainerProfile,
            DeriveAppContainerSidFromAppContainerName,
        },
        PSID,
    },
};

use super::{
    Access, CommandLaunch, CommandProcess, NativeError, OperationPaths, acl, launch, wide,
};

pub struct OperationProfile {
    name: Vec<u16>,
    sid: PSID,
    deleted: bool,
}

impl OperationProfile {
    pub fn create(operation_id: [u8; 16]) -> Result<Self, NativeError> {
        let name = format!("morons-{}", hexadecimal(&operation_id));
        let name = wide(OsStr::new(&name))?;
        let display = wide(OsStr::new("Morons sandbox operation"))?;
        let mut sid = std::ptr::null_mut::<c_void>();
        // SAFETY: All strings are NUL-terminated and live through the call, no
        // capabilities are supplied, and `sid` is a valid out-parameter.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                display.as_ptr(),
                display.as_ptr(),
                std::ptr::null(),
                0,
                &mut sid,
            )
        };
        if result < 0 {
            if result != hresult_from_win32(ERROR_ALREADY_EXISTS) {
                return Err(NativeError::code("profile-create", result as u32));
            }
            // SAFETY: `name` is NUL-terminated and `sid` is a valid
            // out-parameter that is released by this type.
            let derived =
                unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
            if derived < 0 {
                return Err(NativeError::code("profile-derive", derived as u32));
            }
        }
        if sid.is_null() {
            return Err(NativeError::code("profile-sid", 0));
        }
        Ok(Self {
            name,
            sid,
            deleted: false,
        })
    }

    pub fn grant_operation(&self, paths: OperationPaths<'_>) -> Result<(), NativeError> {
        acl::grant(paths.candidate, self.sid, Access::ReadWriteExecute, true)?;
        acl::grant(paths.runtime, self.sid, Access::ReadWriteExecute, true)?;
        acl::grant(paths.image, self.sid, Access::ReadExecute, true)
    }

    pub fn launch_command(&self, launch: CommandLaunch<'_>) -> Result<CommandProcess, NativeError> {
        launch::launch(self.sid, launch)
    }

    pub fn delete(mut self) -> Result<(), NativeError> {
        self.delete_inner()
    }

    fn delete_inner(&mut self) -> Result<(), NativeError> {
        if self.deleted {
            return Ok(());
        }
        // SAFETY: `name` remains a valid NUL-terminated profile name for this
        // object's lifetime.
        let result = unsafe { DeleteAppContainerProfile(self.name.as_ptr()) };
        if result < 0 {
            return Err(NativeError::code("profile-delete", result as u32));
        }
        self.deleted = true;
        Ok(())
    }
}

impl Drop for OperationProfile {
    fn drop(&mut self) {
        let _ = self.delete_inner();
        if !self.sid.is_null() {
            // SAFETY: the SID was allocated by an AppContainer profile API and
            // is released exactly once when this owner is dropped.
            unsafe {
                let _ = FreeSid(self.sid);
            }
            self.sid = std::ptr::null_mut();
        }
    }
}

fn hresult_from_win32(code: u32) -> i32 {
    if code == 0 {
        0
    } else {
        ((code & 0x0000_ffff) | 0x8007_0000) as i32
    }
}

fn hexadecimal(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{hexadecimal, hresult_from_win32};
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;

    #[test]
    fn profile_name_components_are_canonical() {
        assert_eq!(
            hexadecimal(&[0x00, 0x01, 0xfe, 0xff]),
            "0001feff".to_owned()
        );
        assert_eq!(hresult_from_win32(ERROR_ALREADY_EXISTS) as u32, 0x8007_00b7);
    }
}
