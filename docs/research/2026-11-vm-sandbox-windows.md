# Sandboxing the VM process on Windows

Date: 2026-11-12
Verdict: **DRAWBACKS** (details at the end). A restricted token and a set of process mitigation policies work with
the Windows Hypervisor Platform (WHP) and pass the whole Windows VM test suite. The stronger controls (low
integrity, AppContainer) also run WHP, but each one needs Puddle and msb to change how they create and label
files, pipes and folders, and several mitigation policies cannot be used with msb as it is built today.

This is research. No product code changed.

## The problem

On Windows, a workspace's VM runs in `msb.exe machine`, the msb runtime that hosts libkrun's VMM over WHP. Today
that process runs with the user's full token: no restricted token, no low integrity level, no AppContainer, no
mitigation policy. The SDK's job object and Puddle's job object set only `KILL_ON_JOB_CLOSE`. A guest that finds
one device bug in the VMM gets code execution as the user, with the user's files, credentials and every other
workspace in reach.

## How this was measured

- **Windows 11 Enterprise 25H2 (build 26200)**, Intel Core Ultra 7 265H, a standard user with UAC (medium
  integrity; the Administrators group is deny-only), not domain-joined, Defender on, virtualization-based security
  and HVCI on, on mains power. Throwaway files lived in a `%TEMP%\puddle-sandbox-*` folder (tools, an msb runtime, several
  msb homes); nothing outside that folder and the current user's AppContainer profile list changed, and no driver
  was installed.
- **GitHub's `windows-2025` runner (Windows Server 2025 Datacenter, build 26100)**, AMD EPYC 9V74, the runner user
  is an administrator (high integrity). Runs 43120807312, 43121803752, 43123622465 and 43129456499 of the
  `vm-windows` workflow on a research branch that never lands.
- The msb runtime is the pinned fork release `v0.7.12-puddle.3` (checksums from `ci/msb-runtime.sha256`).
- Tools written for this (a launcher, a WHP probe, a security-descriptor dump, a capability-name finder, a DLL
  loader), all throwaway:
  - **launcher**: starts a command with a chosen token (restricted, restricting SIDs, low, untrusted, AppContainer,
    LPAC, capabilities), process mitigation attributes, a child-process policy and job limits.
  - **WHP probe**: `WHvGetCapability`, `WHvCreatePartition`, `WHvSetupPartition`, `VirtualAlloc` + touch of guest RAM,
    `WHvMapGpaRange` with `TrackDirtyPages`, `WHvCreateVirtualProcessor`, a vCPU run to `HLT`,
    `WHvQueryGpaRangeDirtyBitmap`, `DiscardVirtualMemory` (what the balloon does) and `MEM_DECOMMIT` (what
    virtio-mem does); it prints the token it runs with.
  - **SDK trial patch** (research branch only): the msb SDK starts the runtime suspended, swaps its primary token
    (`NtSetInformationProcess(ProcessAccessToken)`), then resumes it. This gives the real shape: Puddle's host
    at the user's level, only the runtime restricted. With it, the whole VM test suite ran under each control.
  - **IFEO**: on the CI runner, `Set-ProcessMitigation -Name msb.exe` puts mitigation policies on every `msb.exe`
    the kernel creates, so the real runtime gets them without a code change.

## 1. What msb.exe needs

From a Process Monitor capture of the runtime (msb.exe events only) during the boot of twelve feature tests,
the process list, the libkrun source and the test runs below.

- **Processes.** The SDK (inside Puddle) starts one `msb.exe machine --name … --config-file <runtime dir>\.tmpXXXX`
  per sandbox. The trace shows no process creation by the runtime. The msb **CLI** does start children
  (`msb.exe __launch-protocol` to probe the runtime), and so does Puddle's doctor test boot (see the
  `DisallowChildProcessCreation` result below).
- **DLLs.** `WinHvPlatform.dll`, `vid.dll`, `WinHvEmulation.dll`, `libkrunfw.dll` (from the runtime folder,
  unsigned), and through its imports `user32`, `gdi32`, `shell32`, `ole32`, `ws2_32`, `dnsapi`, `crypt32`,
  `bcrypt`, `iphlpapi`, `cfgmgr32`.
- **WHP.** One partition per sandbox through `\\?\VidExo`: `WHvCreatePartition`, `WHvSetPartitionProperty`,
  `WHvSetupPartition`, `WHvMapGpaRange`/`WHvUnmapGpaRange`, `WHvQueryGpaRangeDirtyBitmap`, virtual-processor
  create/run/cancel/registers/state, `WHvRequestInterrupt`, `WHvTranslateGva`, and the WHP instruction emulator.
  Guest RAM is ordinary private memory (`VirtualAlloc`), the balloon frees with `DiscardVirtualMemory`, virtio-mem
  maps and decommits ranges.
- **Files, all under the msb home unless named.** The SDK's SQLite state (`db\msb.db`, `-wal`, `-shm`, opened
  read-write), lock files (`run\locks`, `run\creation-locks`), image cache (`cache\layers\*.erofs`,
  `cache\fsmeta`, `cache\vmdk`), the sandbox folder (`runtime\` with the launch config, `guest-state\`, `logs\`,
  `checkpoints\`, `checkpoint-store\`, `upper.ext4` or qcow2 root disk, `external-mounts.json`), the guest share
  (`guest-share\…`, with NTFS alternate data streams `:msb.override_stat`), the user's temp folder (the agent
  bootstrap writes a temp file there), and every bind folder Puddle mounts, which can be anywhere on disk (the
  trace shows a checkout under `%TEMP%`). It also issues `FSCTL`s (sparse files, zeroing) and volume queries.
- **Named pipes it creates:** `msb-agent-<hash>.control.sock`, `msb-agent-console-N-<hash>`,
  `msb-agent-bulk-console-N-<hash>` (it is the server; the host connects). It also connects to pipes the host
  creates (startup and parent-watch pipes), and the metrics shared memory `Local\<name>` comes from the host.
- **Host sockets.** Every vsock route Puddle sets (the guest's only way out: proxy, git, logins, capture) is a
  host socket path the runtime connects to when the guest opens that port.
- **Registry.** Only loader and locale reads (`HKLM\…\Image File Execution Options`, `Safer\CodeIdentifiers`,
  `GRE_Initialize`, `HKCU\Control Panel\Desktop`).
- **Network.** None from the runtime itself (no network device; traffic leaves through the host's proxy).
- **Not covered by the trace.** Under Process Monitor the guests did not reach agent readiness within 180 s, so the
  capture covers boot, not live memory resize, disk grow, snapshots, capture, git, SSH, port forwards or VS Code.
  Those features were covered instead by running the whole test suite under each control (next section).

## 2. The controls, one by one

### The WHP probe under every control

Same result on Windows 11 and on Windows Server 2025:

| Control | WHP |
|---|---|
| Restricted token (`DISABLE_MAX_PRIVILEGE`, Administrators and local-admin SIDs deny-only) | works |
| Restricting SIDs (user, logon, Everyone, Users, Authenticated Users, `RESTRICTED`, `INTERACTIVE`) | works |
| Low integrity | works |
| Untrusted integrity | works (the probe imports no `user32`) |
| AppContainer, no capability | **fails**: `WHvGetCapability` returns `E_ACCESSDENIED` (0x80070005) |
| AppContainer or LPAC with capability `hypervisorPlatform` | works |
| Every mitigation attribute tried (ACG, signature, Win32k, extension points, CFG, strict CFG, fonts, image load, strict handles, heap, ASLR, CET, CET strict, FSCTL, loader integrity, module tampering), child-process block, job UI limits and one active process | works |
| `block-non-cet` | the probe itself is refused (not built `/CETCOMPAT`) |

Why AppContainer needs a capability: `\\?\VidExo` carries a security descriptor hard-coded in `Vid.sys`:
full access for SYSTEM and Administrators, read, write and execute for Authenticated Users and for one capability
SID (`S-1-15-3-1024-1350733694-…-669908847`), and a low no-write-up label. Deriving capability SIDs for every string in the Hyper-V binaries matched that SID to the named
capability `hypervisorPlatform`. An unpackaged process gets it by naming it in its `SECURITY_CAPABILITIES`; no
manifest or Store signature was needed. An AppContainer process also needs a registered profile
(`CreateAppContainerProfile`, per user); without one `CreateProcess` fails with `ERROR_FILE_NOT_FOUND`.

The probe's timings did not move with any control (256 MiB: 21 to 30 ms commit and touch, 2 to 3 ms map,
under 1 ms vCPU run, 67 to 88 ms discard).

### The real runtime, whole test suite (CI, runtime-only token through the SDK patch)

154 tests in 4 partitions per trial (the nightly filter, without the desktop VS Code tests, which ran separately).

| Trial | Result |
|---|---|
| base (no control) | 154 of 154 pass |
| restricted token | 154 of 154 pass |
| restricting SIDs | 154 of 154 pass |
| `strong`: IFEO mitigations on msb.exe (no child processes, ACG, no extension points, system fonts only, no remote or low-label images, prefer System32, strict handles, forced and bottom-up high-entropy ASLR, SEHOP, terminate on heap corruption, CET user shadow stacks strict) plus job UI limits and one active process | 151 of 154 pass; the 3 failures are the doctor's test boot, where an `msb.exe` starts `msb.exe __launch-protocol` and Windows refuses the child (`ERROR_CHILD_PROCESS_BLOCKED`, 367) |
| low integrity (msb home and a runtime-only `TEMP` labelled low) | 112 of 154 pass. VMs boot, run commands, resize memory, grow and trim disks; the 42 failures are everything that goes from the guest to the host through a vsock route (proxy egress, git over HTTPS and SSH, capture, logins, the hostile-guest route sweeps): "Connection reset by peer". The routes connect to sockets the host creates without a low label, which a low process may not write to |
| AppContainer + `hypervisorPlatform` (msb home granted to `ALL APPLICATION PACKAGES` before every start) | 20 of 154 pass. The first try failed every boot at the launch config ("failed to read --config-file … Access is denied"; the SDK writes it with an owner-only descriptor). With the grant re-applied after the config is written, the runtime starts, creates its agent pipe, and then fails at the SDK's SQLite database ("database connect: … Access is denied") |
| desktop VS Code attach | passes under base, restricted and `strong` |

Notes on the restricted trials: on the runner the user is an administrator, so the folders it creates grant
Administrators, which a restricted token holds deny-only. The trial granted the user's own SID on the runner's temp
folder, as a non-admin install already has it. Without that grant the runtime fails at `msb.db.migration.lock`
(access denied). On the Windows 11 machine, where Puddle runs as a standard user, no grant was needed.

### The runtime under each mitigation, one at a time (CI, IFEO, msb CLI and runtime both affected)

| Mitigation | Result |
|---|---|
| `BlockDynamicCode` (ACG) | works |
| `DisableExtensionPoints` | works |
| `DisableNonSystemFonts` | works |
| `BlockRemoteImageLoads`, `BlockLowLabelImageLoads`, `PreferSystem32` | works |
| `StrictHandle`, ASLR (`ForceRelocateImages`, `BottomUp`, `HighEntropy`), `SEHOP`, `TerminateOnError` | works |
| `CFG` | works, but does nothing for msb code: msb.exe and libkrunfw.dll are not built with CFG (`GuardCFFunctionCount` 0) |
| `StrictCFG` | **msb.exe is refused at creation**: error 1657, image does not enable Control Flow Guard |
| `UserShadowStack` + strict mode | works, but does nothing for msb code: neither binary is marked `CETCOMPAT` |
| `MicrosoftSignedOnly` (also with `AllowStoreSignedBinaries`) | **VM start fails**: `libkrunfw.dll` is refused (`LoadLibraryExW` fails, error 577; Security-Mitigations event 12, CodeIntegrity event 3033) |
| `DisableWin32kSystemCalls` | **msb.exe dies at start** (`STATUS_DLL_INIT_FAILED`, 0xC0000142; event 10): msb imports `user32`/`gdi32` through `shell32` |
| `DisallowChildProcessCreation` | works for the runtime; breaks any `msb.exe` used as a CLI (it probes the runtime with a child) |
| FSCTL system-call disable (attribute, Windows 11 only) | **fails**: "failed to create upper.ext4: Access is denied" (sparse-file FSCTLs) |
| `ProcessSystemCallFilterPolicy` | not settable by a user-mode launcher; not tried |

### Tokens with the real msb, CLI level (msb CLI and runtime both under the token)

| Token | Windows 11 (standard user) | Server 2025 (admin runner) |
|---|---|---|
| restricted / restricting SIDs | boots | fails until the user's SID is granted on the msb home |
| low, msb home medium | fails: `msb.db.migration.lock` access denied | same |
| low, msb home and `TEMP` labelled low | boots | boots |
| untrusted | dies at start (0xC0000142) | same |
| AppContainer + capabilities | fails in the CLI (the SDK's `SHGetKnownFolderPath(ProgramData)` fails in an AppContainer); host-side, so not a runtime result | same |

Further facts found on the way:

- **Low integrity needs a low `TEMP`.** The runtime's agent bootstrap creates a temp file (`tempfile::tempfile()`)
  in `%TEMP%`; at low integrity that fails with "bootstrap rootfs: Access is denied" unless the runtime gets a
  low-labelled temp folder.
- **Never low-label a folder with executables.** Windows starts an image at the lower of the parent's integrity and
  the file's label, so test binaries extracted into a low-labelled folder ran at low integrity themselves (they
  then failed to add a user or to create msys named objects). It would also let a low process replace the runtime.
  A low-labelled DLL is refused under `BlockLowLabelImageLoads`.
- **A folder ACE for one specific AppContainer SID locked out low-integrity processes.** After `icacls <dir> /grant
  *S-1-15-2-<package>:(OI)(CI)RX`, a low-integrity (non-AppContainer) process could no longer list the folder
  (`dir` reported "File Not Found") and msb reported "no matching libkrunfw". The same grant to `ALL APPLICATION
  PACKAGES` (S-1-15-2-1) or to Users did not. Reproduced on four fresh folders; the cause is not established.
- **The token must be set at creation.** Swapping it when a debugger sees the process start is refused
  (`STATUS_NOT_SUPPORTED`, 0xC00000BB); only a process created suspended accepts the swap. A wrapper executable
  between the SDK and msb.exe does not work either: the SDK checks that the startup reply comes from the PID it
  spawned (`sandbox startup PID mismatch`).

### Job limits

`JOB_OBJECT_UILIMIT_ALL` and an active-process limit of 1 on the SDK's per-sandbox job passed the whole suite
(part of `strong`). A memory limit was only tried on the probe; a runtime memory cap must leave room for guest RAM
plus the VMM, so its value belongs with the memory settings, not here.

### Not tried

- Desktop or window-station isolation, a separate low-privilege local account (`CreateProcessWithLogonW` needs a
  password or a service), Windows Sandbox or Hyper-V isolation (a VM inside a VM for every workspace), and Defender
  Application Guard (deprecated). Each is far heavier than the token controls that already work.
- Windows 10, Windows 11 Home, ARM64, a domain-joined machine with Group Policy, and any third-party endpoint
  security product. Defender on the test machine injects nothing into user processes; EDR products that inject
  their own DLLs would be refused by `MicrosoftSignedOnly` (not proposed) and possibly by `BlockDynamicCode` if
  their hooks write code. That is unmeasured.

## 3. Who applies it

| Where | What it can do | Effort | Survives an msb upgrade |
|---|---|---|---|
| The SDK's spawn (fork patch in `sdk/rust/lib/runtime/spawn.rs`) | token (create suspended, swap, resume: works on stable Rust), job limits; an attribute list for mitigations or AppContainer needs either its own `CreateProcessW` call or Rust's unstable `raw_attribute` | small for the token (the research patch is about 250 lines with the AppContainer path), medium for attributes | only while the fork carries the patch; upstream is a candidate because Linux needs the same seam |
| Self-restriction in the runtime (`SetProcessMitigationPolicy` early in `msb machine`) | ACG, extension points, image-load, font, strict handle, child-process blocking for itself; no token change | small | fork patch |
| Puddle's launcher before `CreateProcess` | not possible today: Puddle does not spawn msb.exe, the SDK does, and a wrapper process breaks the SDK's PID check | — | — |
| IFEO (`Set-ProcessMitigation -Name msb.exe`) | everything, with no code | trivial, but needs an administrator and applies by file name to every `msb.exe` on the machine, including the CLI | yes, but it is a machine-wide setting an installer would own |

## 4. Cost

- **Boot.** Restricted, restricting SIDs and `strong` changed no test's time beyond runner noise: over the 151
  tests that passed in all four trials, the summed time was 0.952, 0.958 and 1.002 of base, and the median per-test
  ratio (tests over 5 s) 0.938, 0.968 and 0.998. CLI-level boot on the runner (10 runs each, median): base 1305 to
  1360 ms, low 1317 to 1328 ms, `strong` mitigations 1341 ms, `strong` plus low 1340 ms. On the Windows 11 machine
  (shared with other work, noisier): base 977 ms, restricted 969 ms, restricting SIDs 979 ms.
- **Memory and disk.** The probe's commit, map, discard and decommit times did not change under any control
  (above); the disk tests (root-disk growth, trim, sparse root disk) passed in the restricted and `strong` trials
  in the same time as base.
- **Not measured:** a dedicated throughput benchmark of the guest share under each control. The share is served
  inside the same process, so no control adds a boundary on its path; the share tests passed in the same time.

## 5. What it protects against

The attack is code execution inside the VMM (a device bug such as the balloon range bug).

- **Restricted token with restricting SIDs** (works today, all tests pass): drops all privileges and admin rights.
  It does **not** keep the attacker away from the user's files: the user's SID stays in both the normal and the
  restricting set, so everything the user can open, the runtime can open. Its value is small on a standard-user
  machine and real on an elevated or admin-run one.
- **Mitigations in `strong`**: make the step from a memory-corruption bug to code execution harder (no new
  executable memory, no injected DLLs, no child process, strict handles). They do not limit what code that runs
  can reach.
- **Low integrity**: the runtime can still read everything the user can read (no read-up protection by default),
  but can write only low-labelled objects: the user's documents, profile, credential files, other programs and the
  registry outside `HKCU\Software\AppDataLow` are out of write reach. It needs every folder the runtime writes
  (msb home, its temp, writable shares) to be low-labelled, which makes those folders writable by every other
  low-integrity process on the machine (browser sandboxes, for example) and is the reason this is a drawback.
- **AppContainer + `hypervisorPlatform`**: the strongest: no access to anything that does not grant the package
  SID or `ALL APPLICATION PACKAGES` (read access to the user's files included), its own named-object namespace, no
  network without a capability. LPAC also drops `ALL APPLICATION PACKAGES`. It needs every object the runtime
  touches to grant the package SID: the msb home and its owner-only security descriptors, the runtime folder,
  every bind folder anywhere on disk, the host's pipes and the metrics shared memory (an AppContainer resolves
  `Local\` in its own namespace).
- **Residual risk with any of these:** the attacker keeps the guest's own data, the msb state database (opened
  read-write), the other sandboxes' files under the same msb home unless each sandbox gets its own package SID or
  label, the host's pipes it is allowed to open, and the WHP device. A kernel bug reached through `\\?\VidExo` or
  another driver is out of scope of every control here.

## 6. Degrade story

Each control can fail on a user's machine for a reason Puddle cannot change, and the workspace must still start:

- **Token swap refused** (`NtSetInformationProcess` fails, or a policy forbids it): start the runtime unrestricted
  and report a named doctor problem, "VM process runs without its sandbox: <reason>".
- **AppContainer profile cannot be registered** (policy) or `hypervisorPlatform` not granted: fall back to the
  restricted token, report it.
- **A mitigation refused by the machine** (`CreateProcess` fails with 1657, 577, 0xC0000142 or 367 when the policy is
  on): retry once without that mitigation and report which one, so a security product or an older Windows build
  does not stop the workspace. The doctor test boot must use the same policy as a real boot.
- **Admin-run Puddle** (folders granting only Administrators): grant the user's SID on the msb home at setup, as
  the restricted trial did, or report it.

None of this is built. The doctor has a place for it (`crates/puddle-api/src/routes/doctor.rs`), and the
existing doctor test boot is the natural check.

## Verdict: DRAWBACKS

| # | Drawback | Severity | Workaround researched | How well proven | Open |
|---|---|---|---|---|---|
| 1 | Restricted token and restricting SIDs leave the user's files fully reachable | high (it is most of the threat) | low integrity or AppContainer (below) | measured: all tests pass, access unchanged by design | none |
| 2 | AppContainer needs the package SID on every folder, file, pipe and shared section the runtime uses, including user bind folders anywhere on disk, and the SDK's owner-only descriptors drop inherited grants | high (blocks the strongest control) | grant at setup and per mount; SDK and Puddle add the SID when they write protected descriptors; host-side `Local\` objects moved to handles or `AppContainerNamedObjects` | WHP proven to work; the whole suite fails without the grants (startup config unreadable); measured: 20 of 154 tests pass with the msb home granted, the next blocker is the SQLite state database | the full list of grant sites, and how a bind folder on another user's or a network drive is granted |
| 3 | Low integrity needs low labels on the msb home, the runtime's temp, writable shares and every host socket a vsock route connects to, which opens them to every low process on the machine | medium | per-sandbox labelled folders under the msb home only; a dedicated `TEMP` for the runtime; read-only binds need no label; the host creates its route sockets with a low label (or hands the runtime an open handle) | measured: 112 of 154 pass; all 42 failures are guest-to-host routes | whether other low processes on a user's machine make this worse than today |
| 4 | `MicrosoftSignedOnly` impossible: `libkrunfw.dll` and msb are Puddle's own builds | low (other image-load policies work) | none (would need Microsoft's signature); use `PreferSystem32` + no remote/low-label images | measured (577, events 12 and 3033) | none |
| 5 | `DisableWin32kSystemCalls` kills msb (it imports `user32` through `shell32`) | medium (Win32k is a large kernel attack surface) | remove the `shell32`/`user32` dependency from the runtime build (known-folder and shell calls) | measured (0xC0000142, event 10) | how many crates pull `user32` in |
| 6 | `StrictCFG` refuses msb; `CFG` and CET do nothing for msb code | low | build msb and libkrunfw with `-C control-flow-guard` and `/CETCOMPAT` in the fork's release job | measured (1657; PE headers) | build and test of such a release |
| 7 | `DisallowChildProcessCreation` breaks every msb.exe used as a CLI, including the doctor's test boot | low | apply it to the runtime only (SDK attribute or self-restriction in `msb machine`), not by image name | measured (3 doctor tests, 367) | none |
| 8 | FSCTL system-call disable breaks the sparse root disk | low (drop that policy) | none needed | measured on Windows 11 | none |
| 9 | The controls need a seam in the fork's SDK (suspended spawn and token swap, or an attribute list); a Puddle-side wrapper cannot work | medium (fork upkeep) | upstreamable spawn hook, also needed on Linux | prototype passes the whole suite | upstream's interest |
| 10 | Untested: Windows 10, Windows 11 Home, ARM64, domain policy, third-party EDR | unknown | the degrade path in section 6 | not measured | all of it |

**What could start now without these drawbacks** (an implementation row decides this, nothing is built): the
restricted token with restricting SIDs plus the `strong` mitigations applied to the runtime alone (not by image
name), with the degrade path of section 6. It passed every VM test on Windows Server 2025, boots in the same time,
and runs WHP on Windows 11. It does not close drawback 1; that needs low integrity or AppContainer, and each of those
needs the folder and pipe work in drawbacks 2 and 3 first.
