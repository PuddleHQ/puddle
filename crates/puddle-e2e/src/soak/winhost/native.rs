// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Win32 calls behind the Windows readings: the process list with thread counts, one
//! process's working set and handle count, the machine's memory and commit charge, CPU time and
//! free disk.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::Threading::{
    GetProcessHandleCount, GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::{Memory, Proc};

/// Every process of the machine.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Win32 FFI: std has no call that lists processes"
)]
pub fn snapshot() -> Vec<Proc> {
    // SAFETY: CreateToolhelp32Snapshot takes plain values and returns a new handle or
    // INVALID_HANDLE_VALUE.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    // SAFETY: `raw` is a snapshot handle just returned to us that nothing else owns; the
    // OwnedHandle closes it once.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
        ..PROCESSENTRY32W::default()
    };
    let mut all = Vec::new();
    // SAFETY: per Microsoft Learn (Process32FirstW), a process snapshot handle and an entry whose
    // dwSize is its own size.
    // https://learn.microsoft.com/en-us/windows/win32/api/tlhelp32/nf-tlhelp32-process32firstw
    let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    while more {
        let len = entry
            .szExeFile
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(entry.szExeFile.len());
        all.push(Proc {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            exe: String::from_utf16_lossy(entry.szExeFile.get(..len).unwrap_or_default()),
            threads: entry.cntThreads,
        });
        // SAFETY: as for Process32FirstW: the same snapshot and entry.
        more = unsafe { Process32NextW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    all
}

/// The working set in KiB and the open handle count of `pid`; `None` for a process this user may
/// not query or that has ended.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Win32 FFI: std has no call that reads another process's memory or handle count"
)]
pub fn process(pid: u32) -> Option<(f64, f64)> {
    // SAFETY: OpenProcess takes plain values and returns a new handle or null.
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a process handle just returned to us that nothing else owns.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..PROCESS_MEMORY_COUNTERS::default()
    };
    // SAFETY: per Microsoft Learn (GetProcessMemoryInfo), a handle with
    // PROCESS_QUERY_LIMITED_INFORMATION and a writable structure whose cb is its own size.
    // https://learn.microsoft.com/en-us/windows/win32/api/psapi/nf-psapi-getprocessmemoryinfo
    let got =
        unsafe { K32GetProcessMemoryInfo(handle.as_raw_handle(), &raw mut counters, counters.cb) };
    if got == 0 {
        return None;
    }
    let mut handles = 0_u32;
    // SAFETY: per Microsoft Learn (GetProcessHandleCount), a process handle and a writable u32.
    // https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesshandlecount
    let counted = unsafe { GetProcessHandleCount(handle.as_raw_handle(), &raw mut handles) };
    if counted == 0 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "sizes below 2^52")]
    Some((counters.WorkingSetSize as f64 / 1024.0, f64::from(handles)))
}

/// The machine's memory.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Win32 FFI: std has no call that reads the machine's memory status"
)]
pub fn memory() -> Option<Memory> {
    let mut status = MEMORYSTATUSEX {
        dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).ok()?,
        dwMemoryLoad: 0,
        ullTotalPhys: 0,
        ullAvailPhys: 0,
        ullTotalPageFile: 0,
        ullAvailPageFile: 0,
        ullTotalVirtual: 0,
        ullAvailVirtual: 0,
        ullAvailExtendedVirtual: 0,
    };
    // SAFETY: per Microsoft Learn (GlobalMemoryStatusEx), a writable MEMORYSTATUSEX whose
    // dwLength is its own size. The "page file" figures are the commit limit and the commit left.
    // https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-globalmemorystatusex
    let ok = unsafe { GlobalMemoryStatusEx(&raw mut status) };
    (ok != 0).then_some(Memory {
        avail_phys: status.ullAvailPhys,
        commit_limit: status.ullTotalPageFile,
        commit_avail: status.ullAvailPageFile,
    })
}

fn ticks(t: FILETIME) -> u64 {
    (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
}

/// Busy and total CPU time of the machine so far, in 100 ns ticks.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Win32 FFI: std has no call that reads the machine's CPU times"
)]
pub fn cpu_ticks() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: per Microsoft Learn (GetSystemTimes), three writable FILETIMEs.
    // https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getsystemtimes
    let ok = unsafe { GetSystemTimes(&raw mut idle, &raw mut kernel, &raw mut user) };
    if ok == 0 {
        return None;
    }
    // Kernel time includes the idle time.
    let total = ticks(kernel) + ticks(user);
    Some((total.saturating_sub(ticks(idle)), total))
}

/// Free KiB for this user on the volume holding `path`.
#[must_use]
#[expect(
    unsafe_code,
    reason = "Win32 FFI: std has no call that reads a volume's free space"
)]
pub fn free_disk_kb(path: &Path) -> Option<f64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0_u64;
    // SAFETY: per Microsoft Learn (GetDiskFreeSpaceExW), a NUL-terminated path, one writable u64
    // and two optional null pointers.
    // https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getdiskfreespaceexw
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    #[expect(clippy::cast_precision_loss, reason = "sizes below 2^52")]
    (ok != 0).then(|| free as f64 / 1024.0)
}
