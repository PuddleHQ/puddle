// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway: does WHP work in this process (token, integrity, AppContainer, mitigations)?
// Each step prints ok or the HRESULT; exit 0 only when a vCPU ran to HLT.
use std::time::Instant;
use windows_sys::Win32::System::Hypervisor::*;
use windows_sys::Win32::System::Memory::*;

fn step(name: &str, hr: i32) -> bool {
    if hr >= 0 {
        println!("  {name}: ok");
        true
    } else {
        println!("  {name}: FAIL hr=0x{:08x}", hr as u32);
        false
    }
}

unsafe fn whoami() {
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Threading::*;
    let mut tok = std::ptr::null_mut();
    OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok);
    let mut buf = vec![0u8; 256];
    let mut len = 0u32;
    GetTokenInformation(tok, TokenIntegrityLevel, buf.as_mut_ptr().cast(), 256, &mut len);
    let lbl = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
    let n = *GetSidSubAuthorityCount(lbl.Label.Sid) as u32;
    let rid = *GetSidSubAuthority(lbl.Label.Sid, n - 1);
    let mut ac = 0u32;
    GetTokenInformation(tok, TokenIsAppContainer, (&mut ac as *mut u32).cast(), 4, &mut len);
    println!("  token: integrity=0x{rid:x} restricted={} appcontainer={ac}", IsTokenRestricted(tok));
}

fn main() {
    let mem_mib: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);
    let t0 = Instant::now();
    unsafe {
        whoami();
        let mut present: u32 = 0;
        let mut written = 0u32;
        let hr = WHvGetCapability(
            WHvCapabilityCodeHypervisorPresent,
            (&mut present as *mut u32).cast(),
            4,
            &mut written,
        );
        if !step("WHvGetCapability(HypervisorPresent)", hr) {
            std::process::exit(2);
        }
        println!("  hypervisor present: {present}");
        let mut part: WHV_PARTITION_HANDLE = 0;
        if !step("WHvCreatePartition", WHvCreatePartition(&mut part)) {
            std::process::exit(3);
        }
        let cpus: u32 = 1;
        if !step(
            "WHvSetPartitionProperty(ProcessorCount)",
            WHvSetPartitionProperty(part, WHvPartitionPropertyCodeProcessorCount, (&cpus as *const u32).cast(), 4),
        ) {
            std::process::exit(4);
        }
        if !step("WHvSetupPartition", WHvSetupPartition(part)) {
            std::process::exit(5);
        }
        // Guest RAM: reserve+commit like a VMM, touch every page, map with dirty tracking.
        let bytes = mem_mib << 20;
        let ta = Instant::now();
        let ram = VirtualAlloc(std::ptr::null(), bytes as usize, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE);
        if ram.is_null() {
            println!("  VirtualAlloc: FAIL {}", std::io::Error::last_os_error());
            std::process::exit(6);
        }
        let p = ram as *mut u8;
        let mut off = 0u64;
        while off < bytes {
            *p.add(off as usize) = 0;
            off += 4096;
        }
        let commit_ms = ta.elapsed().as_secs_f64() * 1e3;
        let tm = Instant::now();
        let ok = step(
            "WHvMapGpaRange(RAM, RWX|TrackDirtyPages)",
            WHvMapGpaRange(
                part,
                ram,
                0,
                bytes,
                WHvMapGpaRangeFlagRead | WHvMapGpaRangeFlagWrite | WHvMapGpaRangeFlagExecute | WHvMapGpaRangeFlagTrackDirtyPages,
            ),
        );
        let map_ms = tm.elapsed().as_secs_f64() * 1e3;
        if !ok {
            std::process::exit(7);
        }
        // Reset vector page: HLT at 0xFFFFFFF0.
        let rv = VirtualAlloc(std::ptr::null(), 4096, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) as *mut u8;
        *rv.add(0xFF0) = 0xF4;
        if !step(
            "WHvMapGpaRange(reset page)",
            WHvMapGpaRange(part, rv.cast(), 0xFFFF_F000, 4096, WHvMapGpaRangeFlagRead | WHvMapGpaRangeFlagExecute),
        ) {
            std::process::exit(8);
        }
        if !step("WHvCreateVirtualProcessor", WHvCreateVirtualProcessor(part, 0, 0)) {
            std::process::exit(9);
        }
        let mut ctx: WHV_RUN_VP_EXIT_CONTEXT = std::mem::zeroed();
        let tr = Instant::now();
        if !step(
            "WHvRunVirtualProcessor",
            WHvRunVirtualProcessor(part, 0, (&mut ctx as *mut WHV_RUN_VP_EXIT_CONTEXT).cast(), std::mem::size_of::<WHV_RUN_VP_EXIT_CONTEXT>() as u32),
        ) {
            std::process::exit(10);
        }
        let run_ms = tr.elapsed().as_secs_f64() * 1e3;
        println!("  exit reason: {} (8 = HLT)", ctx.ExitReason);
        let pages = bytes / 4096;
        let mut bitmap = vec![0u64; pages.div_ceil(64) as usize];
        step(
            "WHvQueryGpaRangeDirtyBitmap",
            WHvQueryGpaRangeDirtyBitmap(part, 0, bytes, bitmap.as_mut_ptr(), (bitmap.len() * 8) as u32),
        );
        // Discard like the balloon does, then decommit like virtio-mem.
        let td = Instant::now();
        let dr = DiscardVirtualMemory(ram, bytes as usize);
        let discard_ms = td.elapsed().as_secs_f64() * 1e3;
        println!("  DiscardVirtualMemory: {}", if dr == 0 { "ok".to_string() } else { format!("FAIL {dr}") });
        let half = bytes / 2;
        let ok = VirtualFree((p.add(half as usize)).cast(), half as usize, MEM_DECOMMIT);
        println!("  VirtualFree(MEM_DECOMMIT half): {}", if ok != 0 { "ok".to_string() } else { format!("FAIL {}", std::io::Error::last_os_error()) });
        WHvDeletePartition(part);
        println!(
            "RESULT {} mem_mib={mem_mib} commit_touch_ms={commit_ms:.1} map_ms={map_ms:.2} run_ms={run_ms:.2} discard_ms={discard_ms:.2} total_ms={:.1}",
            if ctx.ExitReason == WHvRunVpExitReasonX64Halt { "PASS" } else { "FAIL" },
            t0.elapsed().as_secs_f64() * 1e3
        );
        std::process::exit(if ctx.ExitReason == WHvRunVpExitReasonX64Halt { 0 } else { 11 });
    }
}
