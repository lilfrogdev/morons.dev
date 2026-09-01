use std::{ffi::c_void, path::Path};

use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        ACL,
        Authorization::{
            EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, SE_FILE_OBJECT,
            SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
        },
        DACL_SECURITY_INFORMATION, PSID, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    },
    Storage::FileSystem::{FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE},
};

use super::{Access, NativeError, wide};

const DELETE_ACCESS: u32 = 0x0001_0000;

pub(super) fn grant(
    path: &Path,
    sid: PSID,
    access: Access,
    directory: bool,
) -> Result<(), NativeError> {
    if !path.is_absolute() || sid.is_null() {
        return Err(NativeError::code("acl-input", 0));
    }
    let path = wide(path.as_os_str())?;
    let trustee = TRUSTEE_W {
        pMultipleTrustee: std::ptr::null_mut(),
        MultipleTrusteeOperation: 0,
        TrusteeForm: TRUSTEE_IS_SID,
        TrusteeType: TRUSTEE_IS_UNKNOWN,
        ptstrName: sid.cast(),
    };
    let permissions = match access {
        Access::ReadExecute => FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
        Access::ReadWriteExecute => {
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE_ACCESS
        }
    };
    let explicit = EXPLICIT_ACCESS_W {
        grfAccessPermissions: permissions,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: if directory {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            0
        },
        Trustee: trustee,
    };
    let mut old_acl = std::ptr::null_mut::<ACL>();
    let mut descriptor = std::ptr::null_mut::<c_void>();
    // SAFETY: `path` is NUL-terminated, all output pointers are valid, and the
    // returned descriptor is held until the old ACL is no longer used.
    let status = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut old_acl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(NativeError::code("acl-read", status));
    }
    let descriptor = LocalAllocation(descriptor);
    let mut new_acl = std::ptr::null_mut::<ACL>();
    // SAFETY: `explicit` and `old_acl` remain valid through the call and
    // `new_acl` is a valid out-parameter released below.
    let status = unsafe { SetEntriesInAclW(1, &explicit, old_acl, &mut new_acl) };
    if status != 0 {
        return Err(NativeError::code("acl-build", status));
    }
    let new_acl = LocalAllocation(new_acl.cast());
    // SAFETY: `path` and the new ACL remain valid through the call; owner,
    // group, and SACL are intentionally unchanged.
    let status = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_acl.0.cast(),
            std::ptr::null(),
        )
    };
    drop(descriptor);
    if status != 0 {
        return Err(NativeError::code("acl-write", status));
    }
    Ok(())
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this pointer was allocated by a Windows local-allocation
            // security API and this owner releases it exactly once.
            unsafe {
                let _ = LocalFree(self.0);
            }
        }
    }
}
