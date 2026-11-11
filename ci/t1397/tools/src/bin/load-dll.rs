// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway: LoadLibraryW each argument and print the result (for image-load mitigations).
use std::os::windows::ffi::OsStrExt;
#[link(name = "kernel32", kind = "raw-dylib")]
unsafe extern "system" { fn LoadLibraryW(p: *const u16) -> *mut core::ffi::c_void; }
fn main() {
    let mut fail = 0;
    for a in std::env::args().skip(1) {
        let w: Vec<u16> = std::ffi::OsStr::new(&a).encode_wide().chain(Some(0)).collect();
        let h = unsafe { LoadLibraryW(w.as_ptr()) };
        if h.is_null() { fail += 1; println!("load {a}: FAIL {}", std::io::Error::last_os_error()); } else { println!("load {a}: ok"); }
    }
    std::process::exit(fail);
}
