// SPDX-License-Identifier: GPL-3.0-or-later
// Throwaway launcher: start a command under one candidate sandbox control and report how it ends.
// Usage: sbx-launch [--token full|restricted|rsids|low|untrusted] [--swap] [--ac NAME] [--lpac]
//                   [--cap SID|name]... [--mit NAME]... [--child-block] [--job ui|active1|mem:MB]...
//                   -- program args...
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::Instant;

use windows_sys::core::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::Isolation::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::SystemServices::*;
use windows_sys::Win32::System::Threading::*;

#[link(name = "ntdll", kind = "raw-dylib")]
extern "system" {
    fn NtCreateLowBoxToken(out: *mut HANDLE, existing: HANDLE, access: u32, oa: *const core::ffi::c_void, package: PSID, ncap: u32, caps: *const SID_AND_ATTRIBUTES, nh: u32, h: *const HANDLE) -> i32;
    fn NtSetInformationProcess(h: HANDLE, class: u32, info: *const core::ffi::c_void, len: u32) -> i32;
}

fn w(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn die(what: &str) -> ! {
    let e = std::io::Error::last_os_error();
    println!("LAUNCH-FAIL {what}: {e} (code {})", e.raw_os_error().unwrap_or(0));
    std::process::exit(120);
}

unsafe fn sid_from(s: &str) -> PSID {
    let mut sid: PSID = std::ptr::null_mut();
    if s.starts_with("S-") {
        if ConvertStringSidToSidW(w(s).as_ptr(), &mut sid) == 0 {
            die("ConvertStringSidToSidW");
        }
        return sid;
    }
    // A capability name, e.g. internetClient.
    let mut groups: *mut PSID = std::ptr::null_mut();
    let mut ng = 0u32;
    let mut caps: *mut PSID = std::ptr::null_mut();
    let mut nc = 0u32;
    if DeriveCapabilitySidsFromName(w(s).as_ptr(), &mut groups, &mut ng, &mut caps, &mut nc) == 0 || nc == 0 {
        die("DeriveCapabilitySidsFromName");
    }
    *caps
}

unsafe fn token_info(tok: HANDLE, class: i32) -> Vec<u8> {
    let mut len = 0u32;
    GetTokenInformation(tok, class, std::ptr::null_mut(), 0, &mut len);
    let mut buf = vec![0u8; len as usize];
    if GetTokenInformation(tok, class, buf.as_mut_ptr().cast(), len, &mut len) == 0 {
        die("GetTokenInformation");
    }
    buf
}

unsafe fn make_token(mode: &str) -> HANDLE {
    let mut tok: HANDLE = std::ptr::null_mut();
    if OpenProcessToken(
        GetCurrentProcess(),
        TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
        &mut tok,
    ) == 0
    {
        die("OpenProcessToken");
    }
    let deny: Vec<SID_AND_ATTRIBUTES> = ["S-1-5-32-544", "S-1-5-114", "S-1-5-32-578"]
        .iter()
        .map(|s| SID_AND_ATTRIBUTES { Sid: sid_from(s), Attributes: 0 })
        .collect();
    let mut restrict: Vec<SID_AND_ATTRIBUTES> = Vec::new();
    if mode == "rsids" {
        let user = token_info(tok, TokenUser);
        let tu = &*(user.as_ptr() as *const TOKEN_USER);
        restrict.push(SID_AND_ATTRIBUTES { Sid: tu.User.Sid, Attributes: 0 });
        let logon = token_info(tok, TokenLogonSid);
        let tg = &*(logon.as_ptr() as *const TOKEN_GROUPS);
        if tg.GroupCount > 0 {
            restrict.push(SID_AND_ATTRIBUTES { Sid: tg.Groups[0].Sid, Attributes: 0 });
        }
        Box::leak(Box::new(user));
        Box::leak(Box::new(logon));
        for s in ["S-1-1-0", "S-1-5-32-545", "S-1-5-11", "S-1-5-12", "S-1-5-4"] {
            restrict.push(SID_AND_ATTRIBUTES { Sid: sid_from(s), Attributes: 0 });
        }
    }
    let (flags, ndeny) = match mode {
        "restricted" | "rsids" | "restricted-low" => (DISABLE_MAX_PRIVILEGE, deny.len() as u32),
        _ => (0, 0),
    };
    let mut out: HANDLE = std::ptr::null_mut();
    if CreateRestrictedToken(
        tok,
        flags,
        ndeny,
        deny.as_ptr(),
        0,
        std::ptr::null(),
        restrict.len() as u32,
        if restrict.is_empty() { std::ptr::null() } else { restrict.as_ptr() },
        &mut out,
    ) == 0
    {
        die("CreateRestrictedToken");
    }
    let il = match mode {
        "low" | "restricted-low" => Some("S-1-16-4096"),
        "untrusted" => Some("S-1-16-0"),
        _ => None,
    };
    if let Some(il) = il {
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES { Sid: sid_from(il), Attributes: SE_GROUP_INTEGRITY as u32 },
        };
        let len = std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(label.Label.Sid);
        if SetTokenInformation(out, TokenIntegrityLevel, (&label as *const TOKEN_MANDATORY_LABEL).cast(), len) == 0 {
            die("SetTokenInformation(IntegrityLevel)");
        }
    }
    out
}

fn mit_bits(name: &str) -> (u64, u64) {
    match name {
        "dep" => (0x1, 0),
        "sehop" => (0x1 << 4, 0),
        "force-aslr" => (0x1 << 8, 0),
        "heap-terminate" => (0x1 << 12, 0),
        "bottom-up-aslr" => (0x1 << 16, 0),
        "high-entropy-aslr" => (0x1 << 20, 0),
        "strict-handle" => (0x1 << 24, 0),
        "win32k" => (0x1 << 28, 0),
        "extpoint" => (0x1u64 << 32, 0),
        "acg" => (0x1u64 << 36, 0),
        "cfg" => (0x1u64 << 40, 0),
        "msonly" => (0x1u64 << 44, 0),
        "font" => (0x1u64 << 48, 0),
        "no-remote-image" => (0x1u64 << 52, 0),
        "no-low-label-image" => (0x1u64 << 56, 0),
        "prefer-system32" => (0x1u64 << 60, 0),
        // Policy word 2.
        "loader-integrity" => (0, 0x1 << 4),
        "strict-cfg" => (0, 0x1 << 8),
        "module-tamper" => (0, 0x1 << 12),
        "ibp" => (0, 0x1 << 16),
        "ssbd" => (0, 0x1 << 24),
        "cet" => (0, 0x1 << 28),
        "cet-strict" => (0, 0x3 << 28),
        "cet-ctx-ip" => (0, 0x1u64 << 32),
        "block-non-cet" => (0, 0x1u64 << 36),
        "cet-dyn-apis-ooproc" => (0, 0x1u64 << 48),
        "fsctl" => (0, 0x1u64 << 56),
        other => {
            println!("LAUNCH-FAIL unknown mitigation {other}");
            std::process::exit(121)
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let sep = args.iter().position(|a| a == "--").unwrap_or_else(|| {
        println!("usage: sbx-launch [options] -- program args");
        std::process::exit(122)
    });
    let (opts, cmd) = (&args[..sep], &args[sep + 1..]);
    if opts.first().map(String::as_str) == Some("--ac-delete") {
        let hr = unsafe { DeleteAppContainerProfile(w(&opts[1]).as_ptr()) };
        println!("DeleteAppContainerProfile hr=0x{:08x}", hr as u32);
        std::process::exit(0);
    }
    let mut token = "full".to_string();
    let mut swap = false;
    let mut ac: Option<String> = None;
    let mut lpac = false;
    let mut caps: Vec<String> = Vec::new();
    let mut mit = [0u64; 2];
    let mut child_block = false;
    let mut jobs: Vec<String> = Vec::new();
    let mut swap_child: Option<String> = None;
    let mut i = 0;
    while i < opts.len() {
        let v = opts.get(i + 1).cloned().unwrap_or_default();
        match opts[i].as_str() {
            "--token" => { token = v; i += 1 }
            "--swap" => swap = true,
            "--ac" => { ac = Some(v); i += 1 }
            "--lpac" => lpac = true,
            "--cap" => { caps.push(v); i += 1 }
            "--mit" => { for m in v.split(',') { let (a, b) = mit_bits(m); mit[0] |= a; mit[1] |= b; } i += 1 }
            "--child-block" => child_block = true,
            "--job" => { jobs.push(v); i += 1 }
            "--swap-child" => { swap_child = Some(v.to_lowercase()); i += 1 }
            o => { println!("LAUNCH-FAIL unknown option {o}"); std::process::exit(122) }
        }
        i += 1;
    }
    let mut line = String::new();
    for a in cmd {
        if !line.is_empty() { line.push(' ') }
        if a.contains(' ') || a.is_empty() { line.push('"'); line.push_str(a); line.push('"') } else { line.push_str(a) }
    }
    let mut wline = w(&line);

    unsafe {
        // Attribute list.
        let mut nattr = 0u32;
        if mit != [0, 0] { nattr += 1 }
        if child_block { nattr += 1 }
        if ac.is_some() { nattr += 1 }
        if lpac { nattr += 1 }
        let mut size = 0usize;
        InitializeProcThreadAttributeList(std::ptr::null_mut(), nattr.max(1), 0, &mut size);
        let mut attr_buf = vec![0u8; size];
        let attrs = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(attrs, nattr.max(1), 0, &mut size) == 0 {
            die("InitializeProcThreadAttributeList");
        }
        let child_policy: u32 = 1; // PROCESS_CREATION_CHILD_PROCESS_RESTRICTED
        let lpac_policy: u32 = 1; // PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT
        if mit != [0, 0]
            && UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize, mit.as_ptr().cast(), 16, std::ptr::null_mut(), std::ptr::null()) == 0
        {
            die("UpdateProcThreadAttribute(mitigation)");
        }
        if child_block
            && UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY as usize, (&child_policy as *const u32).cast(), 4, std::ptr::null_mut(), std::ptr::null()) == 0
        {
            die("UpdateProcThreadAttribute(child)");
        }
        let mut cap_attrs: Vec<SID_AND_ATTRIBUTES> = caps
            .iter()
            .map(|c| SID_AND_ATTRIBUTES { Sid: sid_from(c), Attributes: SE_GROUP_ENABLED as u32 })
            .collect();
        let mut sc: SECURITY_CAPABILITIES = std::mem::zeroed();
        if let Some(name) = &ac {
            let mut sid: PSID = std::ptr::null_mut();
            let hr = CreateAppContainerProfile(w(name).as_ptr(), w(name).as_ptr(), w("puddle sandbox research").as_ptr(), std::ptr::null(), 0, &mut sid);
            println!("CreateAppContainerProfile hr=0x{:08x} (0x800700b7 = exists)", hr as u32);
            let hr = DeriveAppContainerSidFromAppContainerName(w(name).as_ptr(), &mut sid);
            if hr < 0 {
                println!("LAUNCH-FAIL DeriveAppContainerSidFromAppContainerName hr=0x{:08x}", hr as u32);
                std::process::exit(120);
            }
            let mut s: PWSTR = std::ptr::null_mut();
            ConvertSidToStringSidW(sid, &mut s);
            let mut n = 0;
            while *s.add(n) != 0 { n += 1 }
            println!("appcontainer sid {}", String::from_utf16_lossy(std::slice::from_raw_parts(s, n)));
            sc.AppContainerSid = sid;
            sc.Capabilities = if cap_attrs.is_empty() { std::ptr::null_mut() } else { cap_attrs.as_mut_ptr() };
            sc.CapabilityCount = cap_attrs.len() as u32;
            if UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize, (&sc as *const SECURITY_CAPABILITIES).cast(), std::mem::size_of::<SECURITY_CAPABILITIES>(), std::ptr::null_mut(), std::ptr::null()) == 0 {
                die("UpdateProcThreadAttribute(security capabilities)");
            }
        }
        if lpac
            && UpdateProcThreadAttribute(attrs, 0, PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize, (&lpac_policy as *const u32).cast(), 4, std::ptr::null_mut(), std::ptr::null()) == 0
        {
            die("UpdateProcThreadAttribute(lpac)");
        }

        if let Some(child) = &swap_child {
            let t = if token == "lowbox" {
                let mut base: HANDLE = std::ptr::null_mut();
                OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT, &mut base);
                let mut out: HANDLE = std::ptr::null_mut();
                let st = NtCreateLowBoxToken(&mut out, base, 0xF01FF, std::ptr::null(), sc.AppContainerSid, cap_attrs.len() as u32, cap_attrs.as_ptr(), 0, std::ptr::null());
                if st < 0 { println!("LAUNCH-FAIL NtCreateLowBoxToken status=0x{:08x}", st as u32); std::process::exit(120) }
                out
            } else { make_token(&token) };
            debug_swap(&mut wline, t, child);
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attrs;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let flags = if token == "lowbox" { CREATE_SUSPENDED } else { EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED };
        let t0 = Instant::now();
        let tok = if token == "lowbox" {
            let mut base: HANDLE = std::ptr::null_mut();
            OpenProcessToken(GetCurrentProcess(), TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT, &mut base);
            let mut out: HANDLE = std::ptr::null_mut();
            let st = NtCreateLowBoxToken(&mut out, base, 0xF01FF, std::ptr::null(), sc.AppContainerSid, cap_attrs.len() as u32, cap_attrs.as_ptr(), 0, std::ptr::null());
            if st < 0 { println!("LAUNCH-FAIL NtCreateLowBoxToken status=0x{:08x}", st as u32); std::process::exit(120) }
            // The AppContainer comes from the token here, not from the attribute list.
            si.lpAttributeList = std::ptr::null_mut();
            Some(out)
        } else if token != "full" { Some(make_token(&token)) } else { None };
        let ok = match (tok, swap) {
            (Some(t), false) => CreateProcessAsUserW(t, std::ptr::null(), wline.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 1, flags, std::ptr::null(), std::ptr::null(), &si.StartupInfo, &mut pi),
            _ => CreateProcessW(std::ptr::null(), wline.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 1, flags, std::ptr::null(), std::ptr::null(), &si.StartupInfo, &mut pi),
        };
        if ok == 0 {
            die("CreateProcess");
        }
        if let (Some(t), true) = (tok, swap) {
            // The shape a patched spawner would use: spawn suspended, then swap the primary token.
            #[repr(C)]
            struct ProcessAccessToken { token: HANDLE, thread: HANDLE }
            let pat = ProcessAccessToken { token: t, thread: std::ptr::null_mut() };
            let st = NtSetInformationProcess(pi.hProcess, 9, (&pat as *const ProcessAccessToken).cast(), std::mem::size_of::<ProcessAccessToken>() as u32);
            if st < 0 {
                println!("LAUNCH-FAIL NtSetInformationProcess(ProcessAccessToken) status=0x{:08x}", st as u32);
                TerminateProcess(pi.hProcess, 1);
                std::process::exit(120);
            }
            println!("token swapped before first instruction");
        }
        if !jobs.is_empty() {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            let mut ext: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            ext.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            for j in &jobs {
                if j == "active1" {
                    ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                    ext.BasicLimitInformation.ActiveProcessLimit = 1;
                } else if let Some(mb) = j.strip_prefix("mem:") {
                    ext.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                    ext.ProcessMemoryLimit = mb.parse::<usize>().unwrap() << 20;
                } else if j == "ui" {
                    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS { UIRestrictionsClass: JOB_OBJECT_UILIMIT_ALL };
                    if SetInformationJobObject(job, JobObjectBasicUIRestrictions, (&ui as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(), 4) == 0 {
                        die("SetInformationJobObject(ui)");
                    }
                }
            }
            if SetInformationJobObject(job, JobObjectExtendedLimitInformation, (&ext as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32) == 0 {
                die("SetInformationJobObject(limits)");
            }
            if AssignProcessToJobObject(job, pi.hProcess) == 0 {
                die("AssignProcessToJobObject");
            }
            std::mem::forget(job);
        }
        ResumeThread(pi.hThread);
        WaitForSingleObject(pi.hProcess, INFINITE);
        let mut code = 0u32;
        GetExitCodeProcess(pi.hProcess, &mut code);
        println!("LAUNCH-END exit=0x{code:x} ({}) ms={:.0}", code as i32, t0.elapsed().as_secs_f64() * 1e3);
        let _ = (CreateToolhelp32Snapshot, Thread32First);
        std::process::exit(if code > 255 { 125 } else { code as i32 });
    }
}

// Start the command under a debugger; every new process whose image name ends with `child` (other than the
// root) gets `tok` as its primary token before its first user-mode instruction, then the debugger detaches it.
unsafe fn debug_swap(line: &mut [u16], tok: HANDLE, child: &str) -> ! {
    use windows_sys::Win32::System::Diagnostics::Debug::*;
    use windows_sys::Win32::System::Threading::DEBUG_PROCESS;
    let mut si: STARTUPINFOW = std::mem::zeroed();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
    let t0 = Instant::now();
    if CreateProcessW(std::ptr::null(), line.as_mut_ptr(), std::ptr::null(), std::ptr::null(), 1, DEBUG_PROCESS, std::ptr::null(), std::ptr::null(), &si, &mut pi) == 0 {
        die("CreateProcess(debug)");
    }
    #[repr(C)]
    struct ProcessAccessToken { token: HANDLE, thread: HANDLE }
    let mut ev: DEBUG_EVENT = std::mem::zeroed();
    let mut swapped = 0;
    loop {
        if WaitForDebugEvent(&mut ev, INFINITE) == 0 { die("WaitForDebugEvent") }
        let mut cont = DBG_CONTINUE;
        match ev.dwDebugEventCode {
            CREATE_PROCESS_DEBUG_EVENT => {
                let info = ev.u.CreateProcessInfo;
                let mut buf = [0u16; 1024];
                let mut n = 1024u32;
                QueryFullProcessImageNameW(info.hProcess, 0, buf.as_mut_ptr(), &mut n);
                let name = String::from_utf16_lossy(&buf[..n as usize]).to_lowercase();
                if !info.hFile.is_null() { CloseHandle(info.hFile); }
                if ev.dwProcessId != pi.dwProcessId && name.ends_with(child) {
                    let pat = ProcessAccessToken { token: tok, thread: std::ptr::null_mut() };
                    let st = NtSetInformationProcess(info.hProcess, 9, (&pat as *const ProcessAccessToken).cast(), std::mem::size_of::<ProcessAccessToken>() as u32);
                    println!("swap pid {} {name}: status=0x{:08x}", ev.dwProcessId, st as u32);
                    if st >= 0 { swapped += 1; }
                    ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
                    DebugActiveProcessStop(ev.dwProcessId);
                    continue;
                }
                if ev.dwProcessId != pi.dwProcessId {
                    ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
                    DebugActiveProcessStop(ev.dwProcessId);
                    continue;
                }
            }
            LOAD_DLL_DEBUG_EVENT => { let f = ev.u.LoadDll.hFile; if !f.is_null() { CloseHandle(f); } }
            EXCEPTION_DEBUG_EVENT => {
                let code = ev.u.Exception.ExceptionRecord.ExceptionCode;
                if code != EXCEPTION_BREAKPOINT && code != 0x4000001f { cont = DBG_EXCEPTION_NOT_HANDLED; }
            }
            EXIT_PROCESS_DEBUG_EVENT if ev.dwProcessId == pi.dwProcessId => {
                let code = ev.u.ExitProcess.dwExitCode;
                ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, DBG_CONTINUE);
                println!("LAUNCH-END exit=0x{code:x} ({}) swapped={swapped} ms={:.0}", code as i32, t0.elapsed().as_secs_f64() * 1e3);
                std::process::exit(if code > 255 { 125 } else { code as i32 });
            }
            _ => {}
        }
        ContinueDebugEvent(ev.dwProcessId, ev.dwThreadId, cont);
    }
}
