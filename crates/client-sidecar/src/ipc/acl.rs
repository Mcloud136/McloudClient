//! 命名管道的安全描述符构造。
//!
//! 单独隔离在本模块：仅此处允许 `unsafe`，其余代码保持 `unsafe_code = "deny"`。

#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::iter::once;
use std::os::windows::ffi::OsStrExt;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::error::IpcError;

/// 持有安全描述符与 `SECURITY_ATTRIBUTES`，Drop 时释放。
pub struct PipeSecurity {
    descriptor: *mut c_void,
    attributes: SECURITY_ATTRIBUTES,
}

impl PipeSecurity {
    /// 构造仅允许 `SYSTEM` 与当前用户的保护型 DACL。
    ///
    /// SDDL：`D:P(A;;GA;;;SY)(A;;GA;;;<current_user_sid>)`
    ///
    /// 注意：本地管理员账户的 SID 会被 Windows 规范化为组别名 `LA`，
    /// 即整个本地管理员组；标准用户则保留原 SID。
    pub fn for_current_user() -> Result<Self, IpcError> {
        let sid = current_user_sid()?;
        let sddl = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})");
        let wide: Vec<u16> = OsStr::new(&sddl).encode_wide().chain(once(0)).collect();

        let mut descriptor: *mut c_void = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                1,
                &mut descriptor,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(IpcError::Security(format!(
                "ConvertStringSecurityDescriptor failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        Ok(Self {
            descriptor,
            attributes: SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            },
        })
    }

    /// 传给 `create_with_security_attributes_raw` 的指针。
    pub fn as_mut_ptr(&mut self) -> *mut c_void {
        std::ptr::from_mut(&mut self.attributes).cast()
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }
}

fn current_user_sid() -> Result<String, IpcError> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(IpcError::Security(format!(
                "OpenProcessToken failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
        let mut buffer = vec![0u8; len as usize];
        let ok = GetTokenInformation(token, TokenUser, buffer.as_mut_ptr().cast(), len, &mut len);

        let result = if ok == 0 {
            Err(IpcError::Security(format!(
                "GetTokenInformation failed: {}",
                std::io::Error::last_os_error()
            )))
        } else {
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut sid_string: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid_string) == 0 {
                Err(IpcError::Security(
                    "ConvertSidToStringSidW failed".to_owned(),
                ))
            } else {
                let mut n = 0;
                while *sid_string.add(n) != 0 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, n));
                LocalFree(sid_string as *mut c_void);
                Ok(s)
            }
        };

        CloseHandle(token);
        result
    }
}
