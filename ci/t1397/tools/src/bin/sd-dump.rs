// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway: print the SDDL (owner, DACL, label) of a file, device or pipe path.
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
fn main() {
    for path in std::env::args().skip(1) {
        let wp: Vec<u16> = std::ffi::OsStr::new(&path).encode_wide().chain(Some(0)).collect();
        unsafe {
            let h = CreateFileW(wp.as_ptr(), READ_CONTROL, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut());
            if h == INVALID_HANDLE_VALUE { println!("{path}: open FAIL {}", std::io::Error::last_os_error()); continue; }
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            let info = OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
            let r = GetSecurityInfo(h, SE_KERNEL_OBJECT, info, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), &mut sd);
            if r != 0 { println!("{path}: GetSecurityInfo FAIL {r}"); continue; }
            let mut s: windows_sys::core::PWSTR = std::ptr::null_mut();
            let mut n = 0u32;
            ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, info, &mut s, &mut n);
            println!("{path}: {}", String::from_utf16_lossy(std::slice::from_raw_parts(s, n as usize)));
            CloseHandle(h);
        }
    }
}
