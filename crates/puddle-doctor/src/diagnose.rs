// SPDX-License-Identifier: GPL-3.0-or-later
//! From facts to findings: the order of the checks, what depends on what, and the exact text of
//! every finding and fix. Pure, so every case is tested with a faked [`Probe`].

use std::path::Path;
use std::time::{Duration, Instant};

use puddle_runtime::RuntimeError;

use crate::facts::{
    AgentService, AzCliFacts, BlockedBy, BlockedFolder, BootFacts, CodeIntegrity,
    CodeIntegrityFacts, DataEncryption, DataPathFacts, DiskEncryptionFacts, DiskFacts, GsaFacts,
    HypervisorApi, HypervisorFacts, JobFacts, KeptLogs, Os, ProcessOutcome, Protection,
    RuntimeFacts, RuntimeState, SshAgentFacts, SwapFacts, WritableFolder,
};
use crate::report::{Check, CheckId, Finding, Report, SCHEMA_VERSION, Status};

/// The exit code of the probe program the test VM runs.
pub const PROBE_EXIT_CODE: i32 = 42;

/// Where the facts come from: the real machine ([`crate::SystemProbe`]) or a test's fake.
pub trait Probe {
    /// The OS the findings are worded for.
    fn os(&self) -> Os;
    /// The CPU architecture (`x86_64`, `aarch64`).
    fn arch(&self) -> String;
    /// Hardware virtualization and the hypervisor API.
    fn hypervisor(&self) -> HypervisorFacts;
    /// Windows code integrity; `None` elsewhere.
    fn code_integrity(&self) -> Option<CodeIntegrityFacts>;
    /// The bundled runtime.
    fn runtime(&self) -> RuntimeFacts;
    /// Runs `msb --version`, killed after `limit`. Called only when [`Probe::runtime`] was ready.
    fn launch(&self, limit: Duration) -> ProcessOutcome;
    /// Boots a test VM, killed after `limit`. Called only when the hypervisor is ready and the
    /// launch succeeded.
    fn boot(&self, limit: Duration) -> BootFacts;
    /// The job object this process runs in; `None` when in none (or not on Windows).
    fn job(&self) -> Option<JobFacts>;
    /// The Global Secure Access client; `None` when not on Windows.
    fn global_secure_access(&self) -> Option<GsaFacts>;
    /// The free space on the drive that holds Puddle's data.
    fn disk_space(&self) -> DiskFacts;
    /// The folders above Puddle's data folder; `None` where msb has no such rule (not Linux).
    fn data_path(&self) -> Option<DataPathFacts>;
    /// Whether the drive that holds Puddle's data and the swap are encrypted.
    fn disk_encryption(&self) -> DiskEncryptionFacts;
    /// Asks the SSH agent Puddle would ask for its key list, waiting at most `limit`.
    fn ssh_agent(&self, limit: Duration) -> SshAgentFacts;
    /// Asks the Azure CLI for its version, waiting at most `limit`.
    fn az_cli(&self, limit: Duration) -> AzCliFacts;
}

/// What to run and how long it may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Boot a test VM (the slowest check).
    pub boot: bool,
    /// Time limit for `msb --version`.
    pub launch_limit: Duration,
    /// Time limit for the whole run; the test boot gets what is left.
    pub budget: Duration,
    /// Free space, in bytes, under which the disk check warns.
    pub disk_warn_below: u64,
    /// Free space, in bytes, under which the disk check fails.
    pub disk_fail_below: u64,
    /// Time limit for the SSH agent's answer to a key-list request.
    pub ssh_agent_limit: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            boot: true,
            launch_limit: Duration::from_secs(10),
            budget: Duration::from_secs(28),
            disk_warn_below: DEFAULT_DISK_WARN_BELOW,
            disk_fail_below: DEFAULT_DISK_FAIL_BELOW,
            ssh_agent_limit: SSH_AGENT_LIMIT,
        }
    }
}

/// Puddle's default warning line for free disk space: 20 GB (the settings default is the same).
pub const DEFAULT_DISK_WARN_BELOW: u64 = 20 * GIB;

/// Puddle's default failure line for free disk space: 5 GB (the settings default is the same).
pub const DEFAULT_DISK_FAIL_BELOW: u64 = 5 * GIB;

/// How long the SSH agent gets to answer: an agent that asks the user first (1Password, Bitwarden)
/// is not waited for.
pub const SSH_AGENT_LIMIT: Duration = Duration::from_secs(2);

/// How long `az --version` gets: a cold `az` is slow, and the answer only names a version.
pub const AZ_CLI_LIMIT: Duration = Duration::from_secs(10);

const GIB: u64 = 1024 * 1024 * 1024;

/// The least time worth starting a test boot with.
const MIN_BOOT_TIME: Duration = Duration::from_secs(5);

/// The most time a test boot gets.
const MAX_BOOT_TIME: Duration = Duration::from_secs(25);

/// Runs every check against `probe`.
pub fn diagnose(probe: &dyn Probe, options: &Options, puddle_version: &str) -> Report {
    let start = Instant::now();
    let os = probe.os();
    let hv = probe.hypervisor();
    let mut checks = vec![virtualization(os, &hv), hypervisor(os, &hv)];
    if let Some(ci) = probe.code_integrity() {
        checks.push(code_integrity(&ci));
    }
    let job = probe.job();
    if let Some(check) = job.as_ref().and_then(job_check) {
        checks.push(check);
    }

    let rt = probe.runtime();
    checks.push(runtime(os, &rt));
    let runtime_ready = matches!(rt.state, RuntimeState::Ready { .. });
    let launched = if runtime_ready {
        let outcome = probe.launch(options.launch_limit);
        let check = launch(os, &rt.msb, &outcome, hv.api == HypervisorApi::Ready);
        let ok = check.status == Status::Ok;
        checks.push(check);
        ok
    } else {
        checks.push(skipped(CheckId::Launch, "the bundled runtime isn't usable"));
        false
    };

    checks.push(if !options.boot {
        skipped(CheckId::TestBoot, "--no-boot")
    } else if hv.api != HypervisorApi::Ready {
        skipped(CheckId::TestBoot, "no usable hypervisor")
    } else if !launched {
        skipped(CheckId::TestBoot, "the runtime doesn't start")
    } else {
        let left = options.budget.saturating_sub(start.elapsed());
        if left < MIN_BOOT_TIME {
            skipped(CheckId::TestBoot, "out of time")
        } else {
            let facts = probe.boot(left.min(MAX_BOOT_TIME));
            test_boot(os, &rt.msb, &rt.dir, &facts, job.as_ref())
        }
    });

    checks.push(disk_space(&probe.disk_space(), options));
    if let Some(path) = probe.data_path() {
        checks.push(data_path(&path));
    }
    if os == Os::Windows {
        checks.push(windows_guest_kernel());
    }

    checks.push(disk_encryption(os, &probe.disk_encryption()));

    checks.push(ssh_agent(os, &probe.ssh_agent(options.ssh_agent_limit)));
    checks.push(az_cli(&probe.az_cli(AZ_CLI_LIMIT)));

    if let Some(gsa) = probe.global_secure_access() {
        checks.push(global_secure_access(&gsa));
    }

    Report {
        schema_version: SCHEMA_VERSION,
        puddle_version: puddle_version.to_owned(),
        os: os.name().to_owned(),
        arch: probe.arch(),
        ok: checks.iter().all(|c| c.status != Status::Fail),
        checks,
        elapsed_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn skipped(id: CheckId, why: &str) -> Check {
    Check::new(id, Status::Skipped, format!("not checked: {why}"))
}

// ---- virtualization and hypervisor ----

const FIRMWARE_FIX: &str = "Turn on CPU virtualization in your PC's UEFI/BIOS setup (Intel: \"Intel Virtualization Technology\" / VT-x; \
AMD: \"SVM Mode\"), save, and start the PC again.";

fn virtualization(os: Os, hv: &HypervisorFacts) -> Check {
    let id = CheckId::Virtualization;
    if hv.api == HypervisorApi::Ready {
        return Check::new(id, Status::Ok, "on (the hypervisor uses it)");
    }
    match (hv.firmware_virtualization, &hv.hypervisor_vendor) {
        (Some(true), _) => Check::new(id, Status::Ok, "on in firmware"),
        (_, Some(vendor)) => Check::new(
            id,
            Status::Info,
            format!(
                "this {} runs in a virtual machine ({vendor}); see Hypervisor",
                os_word(os)
            ),
        ),
        (Some(false), None) => Check::new(id, Status::Fail, "off in the firmware (UEFI/BIOS)")
            .finding(Finding::FirmwareVirtualizationOff)
            .fix(FIRMWARE_FIX),
        (None, None) => Check::new(id, Status::Info, "unknown"),
    }
}

fn os_word(os: Os) -> &'static str {
    match os {
        Os::Windows => "Windows",
        Os::Linux => "Linux",
        Os::MacOs => "Mac",
        Os::Other => "system",
    }
}

const WHP_ENABLE_FIX: &str =
    "Enable the Windows Hypervisor Platform feature in PowerShell as administrator:
  Enable-WindowsOptionalFeature -Online -FeatureName HypervisorPlatform -All
then restart Windows. (Or: Settings > System > Optional features > More Windows features > \
Windows Hypervisor Platform.) Virtual Machine Platform, which WSL2 uses, is a different feature.";

const HYPERVISOR_START_FIX: &str =
    "Restart Windows: a newly enabled hypervisor only starts after a restart. If it still doesn't \
run, the boot configuration turns it off; in a terminal as administrator run
  bcdedit /set hypervisorlaunchtype auto
and restart again.";

fn nested_fix(os: Os, vendor: &str) -> String {
    format!(
        "This {} is a virtual machine ({vendor}) whose host doesn't pass CPU virtualization through. \
Turn on nested virtualization for this VM on its host, or run Puddle on a physical PC. On a \
Hyper-V host, with the VM off:
  Set-VMProcessor -VMName <name> -ExposeVirtualizationExtensions $true
On VMware: \"Virtualize Intel VT-x/EPT or AMD-V/RVI\" in the VM's processor settings.",
        os_word(os)
    )
}

fn hypervisor(os: Os, hv: &HypervisorFacts) -> Check {
    let id = CheckId::Hypervisor;
    let windows = os == Os::Windows;
    match &hv.api {
        HypervisorApi::Ready if windows => Check::new(
            id,
            Status::Ok,
            "Windows Hypervisor Platform is on and the hypervisor runs",
        ),
        HypervisorApi::Ready => Check::new(id, Status::Ok, "/dev/kvm is usable"),
        HypervisorApi::NotInstalled { detail } if windows => {
            Check::new(id, Status::Fail, "Windows Hypervisor Platform is off")
                .finding(Finding::WhpNotEnabled)
                .fix(WHP_ENABLE_FIX)
                .detail(detail.clone())
        }
        HypervisorApi::NotInstalled { detail } => Check::new(id, Status::Fail, "no /dev/kvm")
            .finding(Finding::KvmMissing)
            .fix(
                "Load KVM for your CPU (sudo modprobe kvm_intel, or kvm_amd). In a virtual machine, turn on \
nested virtualization on its host first.",
            )
            .detail(detail.clone()),
        HypervisorApi::NotRunning => match (&hv.hypervisor_vendor, hv.firmware_virtualization) {
            (Some(vendor), _) => Check::new(
                id,
                Status::Fail,
                "can't run: this is a virtual machine without nested virtualization",
            )
            .finding(Finding::NestedVirtualizationOff)
            .fix(nested_fix(os, vendor)),
            (None, Some(false)) => Check::new(
                id,
                Status::Fail,
                "Windows Hypervisor Platform is on, but the hypervisor can't run: CPU virtualization is off",
            )
            .finding(Finding::FirmwareVirtualizationOff)
            .fix(FIRMWARE_FIX),
            (None, _) => Check::new(
                id,
                Status::Fail,
                "Windows Hypervisor Platform is on, but the hypervisor isn't running",
            )
            .finding(Finding::HypervisorNotRunning)
            .fix(HYPERVISOR_START_FIX),
        },
        HypervisorApi::AccessDenied { detail } => {
            Check::new(id, Status::Fail, "/dev/kvm exists but you can't open it")
                .finding(Finding::KvmAccessDenied)
                .fix(
                    "Add yourself to the group that owns /dev/kvm (usually kvm):
  sudo usermod -aG kvm \"$USER\"
then log out and in again.",
                )
                .detail(detail.clone())
        }
        HypervisorApi::QueryFailed { detail } => {
            Check::new(id, Status::Fail, "the hypervisor query failed")
                .finding(Finding::HypervisorQueryFailed)
                .fix(if windows {
                    "Restart Windows and run puddle doctor again. If this stays, include the line below when you \
report it."
                } else {
                    "Run puddle doctor again. If this stays, include the line below when you report it."
                })
                .detail(detail.clone())
        }
        HypervisorApi::Unsupported if os == Os::MacOs => Check::new(
            id,
            Status::Fail,
            "Puddle doesn't support macOS yet",
        )
        .finding(Finding::UnsupportedOs)
        .fix("Run Puddle on Windows 10/11 or Linux with KVM for now. macOS on Apple Silicon is being explored."),
        HypervisorApi::Unsupported => Check::new(
            id,
            Status::Fail,
            "Puddle has no hypervisor support on this operating system",
        )
        .finding(Finding::UnsupportedOs)
        .fix("Run Puddle on Windows 10/11 (or Linux with KVM)."),
    }
}

// ---- code integrity ----

fn code_integrity(facts: &CodeIntegrityFacts) -> Check {
    match facts {
        CodeIntegrityFacts::Read(ci) => code_integrity_state(*ci),
        CodeIntegrityFacts::Unreadable { detail } => Check::new(
            CheckId::CodeIntegrity,
            Status::Info,
            "could not be read; whether memory integrity (HVCI) or an App Control policy is on is unknown",
        )
        .finding(Finding::CodeIntegrityUnreadable)
        .fix(
            "Puddle works without it. If \"Runtime starts\" or \"Test boot\" reports a blocked msb, that \
check names the policy.",
        )
        .detail(detail.clone()),
    }
}

fn code_integrity_state(ci: CodeIntegrity) -> Check {
    let hvci = if ci.hvci { "on" } else { "off" };
    let app = if ci.user_mode_enforced {
        "enforced"
    } else if ci.user_mode_audit {
        "audit only"
    } else {
        "off"
    };
    let mut summary =
        format!("memory integrity (HVCI) {hvci}; App Control policy for programs {app}");
    if ci.test_signing {
        summary.push_str("; test signing on");
    }
    let check = Check::new(CheckId::CodeIntegrity, Status::Info, summary);
    if ci.user_mode_enforced {
        check.fix(
            "An App Control (WDAC) policy decides which programs may run. If it blocks Puddle's runtime, \
\"Runtime starts\" says so and names the log with the rule.",
        )
    } else {
        check
    }
}

// ---- job object ----

/// The job object's line; a job that allows breakaway needs none.
fn job_check(job: &JobFacts) -> Option<Check> {
    match job {
        JobFacts::Read {
            breakaway_allowed: true,
        } => None,
        JobFacts::Read {
            breakaway_allowed: false,
        } => Some(
            Check::new(
                CheckId::JobObject,
                Status::Info,
                "Puddle runs in a job object that doesn't let child processes leave it",
            )
            .finding(Finding::JobWithoutBreakaway)
            .fix(
                "Usual under CI runners and some IDE or remote shells. Puddle keeps its VM processes in its own \
job, so this only matters if \"Test boot\" fails with access denied.",
            ),
        ),
        JobFacts::Unreadable { detail } => Some(
            Check::new(
                CheckId::JobObject,
                Status::Info,
                "could not be read; whether Puddle runs in a job object that blocks child processes from leaving it is unknown",
            )
            .finding(Finding::JobUnreadable)
            .fix(
                "Only matters if \"Test boot\" fails with access denied; if it does, run puddle doctor from a \
plain terminal outside a CI runner or IDE.",
            )
            .detail(detail.clone()),
        ),
    }
}

// ---- disk space ----

/// `bytes` in GB (1024 x 1024 x 1024 bytes, as Windows and `df -h` count), with a decimal only
/// when it is not zero.
fn gb(bytes: u64) -> String {
    let tenths = u128::from(bytes) * 10 / u128::from(GIB);
    match tenths % 10 {
        0 => format!("{}", tenths / 10),
        t => format!("{}.{t}", tenths / 10),
    }
}

const DISK_WHY: &str = "Workspace volumes and root disks, package caches and builds inside a workspace all grow on this drive, \
and a full drive stops them with errors that do not name the cause (a build that dies halfway, a tool that cannot write). \
This check measures the room left on the drive, not the sizes of the workspaces' disks: a root disk or a volume is a sparse file, \
so its size is a limit and takes space only as the workspace writes, and the workspaces can still grow past what is left.";

fn disk_space(facts: &DiskFacts, options: &Options) -> Check {
    let id = CheckId::DiskSpace;
    match facts {
        DiskFacts::NoFolder => skipped(id, "Puddle's data folder could not be found"),
        DiskFacts::Unreadable { dir, detail } => Check::new(
            id,
            Status::Info,
            format!(
                "could not be read; how much room the drive that holds {} has is unknown",
                dir.display()
            ),
        )
        .finding(Finding::DiskSpaceUnreadable)
        .fix("Puddle works; this only means it cannot warn you before the drive fills up.")
        .detail(detail.clone()),
        DiskFacts::Read { dir, free_bytes } => {
            let free = gb(*free_bytes);
            let place = format!("on the drive that holds Puddle's data ({})", dir.display());
            if *free_bytes < options.disk_fail_below {
                Check::new(
                    id,
                    Status::Fail,
                    format!(
                        "only {free} GB free {place}, under the {} GB line",
                        gb(options.disk_fail_below)
                    ),
                )
                .finding(Finding::DiskSpaceCritical)
                .fix(format!(
                    "{DISK_WHY} Free space before you start or build anything in a workspace: delete workspaces you no longer \
use and files you do not need. The line is the setting \"Fail when free space is under\" in Settings."
                ))
            } else if *free_bytes < options.disk_warn_below {
                Check::new(
                    id,
                    Status::Warn,
                    format!(
                        "{free} GB free {place}, under the {} GB line",
                        gb(options.disk_warn_below)
                    ),
                )
                .finding(Finding::DiskSpaceLow)
                .fix(format!(
                    "{DISK_WHY} Free some space soon: delete workspaces you no longer use and files you do not need. \
The line is the setting \"Warn when free space is under\" in Settings."
                ))
            } else {
                Check::new(id, Status::Ok, format!("{free} GB free {place}"))
            }
        }
    }
}

// ---- disk encryption ----

const ENCRYPTION_WHY: &str = "Puddle keeps secrets in memory while it runs (the real values behind the stand-ins a workspace holds), \
and memory can be paged out to swap or written to disk in a crash dump. On a drive that is not encrypted, anyone who gets the disk \
(a lost or stolen computer, a disk taken out) can read those pages and the data folder; on an encrypted one they cannot.";

fn encryption_fix(os: Os, data_bad: bool, swap_bad: bool) -> String {
    let how = match os {
        Os::Windows => {
            "Turn on BitLocker or Device encryption for the drive (Settings, Privacy & security, Device encryption; Pro editions: \
Control Panel, BitLocker Drive Encryption). A page file on a drive you leave unencrypted is encrypted by \
`fsutil behavior set encryptpagingfile 1` (run as administrator, then restart)."
        }
        Os::MacOs => {
            "Turn on FileVault (System Settings, Privacy & Security, FileVault). macOS encrypts its swap by default."
        }
        Os::Linux => {
            "Put the drive under LUKS (the installer's \"encrypt the disk\" option; an existing drive needs a backup and a reinstall or \
`cryptsetup reencrypt`). For swap alone, give it a random key each boot: in /etc/crypttab, `swap /dev/<swap partition> /dev/urandom \
swap,cipher=aes-xts-plain64,size=512`, and use /dev/mapper/swap in /etc/fstab; or turn swap off with `sudo swapoff -a` if the machine has the memory."
        }
        Os::Other => "Encrypt the drive and the swap with the tools your OS provides.",
    };
    let what = match (data_bad, swap_bad) {
        (true, true) => "The drive and the swap are",
        (true, false) => "The drive is",
        _ => "The swap is",
    };
    format!("{what} not encrypted. {ENCRYPTION_WHY} {how}")
}

fn protection_text(p: &Protection) -> String {
    match p {
        Protection::Encrypted { by } => format!("encrypted ({by})"),
        Protection::NotEncrypted => "NOT encrypted".to_owned(),
        Protection::NotOnDisk { what } => format!("not on disk ({what})"),
        Protection::Unknown { .. } => "unknown".to_owned(),
    }
}

fn unknown_why(p: &Protection) -> Option<&str> {
    match p {
        Protection::Unknown { why } => Some(why),
        _ => None,
    }
}

fn disk_encryption(os: Os, facts: &DiskEncryptionFacts) -> Check {
    let id = CheckId::DiskEncryption;
    let mut parts = Vec::new();
    let mut protections: Vec<(&str, &Protection)> = Vec::new();
    let mut folder_unknown = None;
    match &facts.data {
        DataEncryption::NoFolder => {
            parts.push("data folder not found".to_owned());
            folder_unknown = Some(
                "Puddle's data folder could not be found, so its drive was not asked".to_owned(),
            );
        }
        DataEncryption::Volume { protection, .. } => {
            parts.push(format!("data drive {}", protection_text(protection)));
            protections.push(("data", protection));
        }
    }
    match &facts.swap {
        SwapFacts::None => parts.push("no swap".to_owned()),
        SwapFacts::Unreadable { .. } => parts.push("swap unknown".to_owned()),
        SwapFacts::Areas(areas) => {
            for a in areas {
                parts.push(format!(
                    "swap {} {}",
                    a.name,
                    protection_text(&a.protection)
                ));
                protections.push(("swap", &a.protection));
            }
        }
    }
    let summary = parts.join("; ");
    let data_bad = protections
        .iter()
        .any(|(k, p)| *k == "data" && **p == Protection::NotEncrypted);
    let swap_bad = protections
        .iter()
        .any(|(k, p)| *k == "swap" && **p == Protection::NotEncrypted);
    let unknowns: Vec<String> = folder_unknown
        .into_iter()
        .chain(
            protections
                .iter()
                .filter_map(|(_, p)| unknown_why(p).map(str::to_owned)),
        )
        .chain(match &facts.swap {
            SwapFacts::Unreadable { why } => Some(why.clone()),
            _ => None,
        })
        .collect();
    let mut unknowns = unknowns;
    unknowns.dedup();
    if data_bad || swap_bad {
        let finding = if data_bad {
            Finding::DiskNotEncrypted
        } else {
            Finding::SwapNotEncrypted
        };
        let mut check = Check::new(id, Status::Warn, summary)
            .finding(finding)
            .fix(encryption_fix(os, data_bad, swap_bad));
        if !unknowns.is_empty() {
            check = check.detail(unknowns.join("; "));
        }
        check
    } else if !unknowns.is_empty() {
        Check::new(id, Status::Info, summary)
            .finding(Finding::DiskEncryptionUnknown)
            .fix("Puddle works; this only means it could not say whether the drive and swap are encrypted.")
            .detail(unknowns.join("; "))
    } else {
        Check::new(id, Status::Ok, summary)
    }
}

// ---- data folder path ----

/// `path` as one shell word: bare when it is plain, in single quotes otherwise.
fn shell_word(path: &Path) -> String {
    let text = path.display().to_string();
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+@:,=".contains(c);
    if !text.is_empty() && text.chars().all(plain) {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

fn data_path(facts: &DataPathFacts) -> Check {
    let id = CheckId::DataPath;
    match facts {
        DataPathFacts::NoFolder => skipped(id, "Puddle's data folder could not be found"),
        DataPathFacts::Unreadable { dir, detail } => Check::new(
            id,
            Status::Info,
            format!(
                "could not be read; whether other accounts can write above {} is unknown",
                dir.display()
            ),
        )
        .finding(Finding::DataPathUnreadable)
        .fix("Puddle may still work; this only means the doctor cannot rule out the folder permissions msb refuses.")
        .detail(detail.clone()),
        DataPathFacts::Checked {
            dir,
            writable,
            blocked,
        } if writable.is_empty() && blocked.is_empty() => Check::new(
            id,
            Status::Ok,
            format!(
                "every folder above {} is a folder only you or root own, and other accounts can't change it",
                dir.display()
            ),
        ),
        DataPathFacts::Checked {
            writable, blocked, ..
        } => data_path_refused(writable, blocked),
    }
}

/// The failure for folders above Puddle's data that msb refuses: `blocked` (not a folder, or
/// another user's) and `writable` (other accounts can rename entries in it).
fn data_path_refused(writable: &[WritableFolder], blocked: &[BlockedFolder]) -> Check {
    let mut problems = Vec::new();
    let mut fixes = Vec::new();
    let mut detail = Vec::new();
    for b in blocked {
        let path = b.path.display();
        let (problem, fix) = match b.by {
            BlockedBy::NotAFolder => (
                format!("{path} is not a folder"),
                format!(
                    "{path} is a file or another kind of entry, but Puddle's data has to live below a folder. \
Rename or remove it (for example `mv {0} {0}.old`), or point XDG_DATA_HOME at a folder you own.",
                    shell_word(&b.path)
                ),
            ),
            BlockedBy::OtherOwner { uid } => (
                format!("{path} belongs to another user (id {uid})"),
                format!(
                    "msb trusts only folders you or root own. If {path} is yours, run `sudo chown \"$USER\" {}`; \
otherwise point XDG_DATA_HOME at a folder you own.",
                    shell_word(&b.path)
                ),
            ),
        };
        problems.push(problem);
        fixes.push(fix);
        detail.push(match b.by {
            BlockedBy::NotAFolder => format!("{path} not a folder"),
            BlockedBy::OtherOwner { uid } => format!("{path} owner id {uid}"),
        });
    }
    if !writable.is_empty() {
        let names = writable
            .iter()
            .map(|f| f.path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let any_world = writable.iter().any(WritableFolder::world_writable);
        let how = if !any_world {
            "writable by the group"
        } else if writable.iter().any(WritableFolder::group_writable) {
            "writable by the group or everyone"
        } else {
            "writable by everyone"
        };
        let flags = if any_world { "go-w" } else { "g-w" };
        let sudo = if writable.iter().all(|f| f.owned) {
            ""
        } else {
            "sudo "
        };
        let words: Vec<String> = writable.iter().map(|f| shell_word(&f.path)).collect();
        problems.push(format!(
            "{names} {} {how} and not sticky",
            if writable.len() == 1 { "is" } else { "are" },
        ));
        fixes.push(format!(
            "msb opens its catalog only below folders where other accounts cannot rename entries, so \
every sandbox start fails until the write permission is gone. A user created with a private group and umask \
0002 (the Ubuntu default) often leaves ~/.local at 0775. Run:
  {sudo}chmod {flags} {}",
            words.join(" ")
        ));
        detail.extend(
            writable
                .iter()
                .map(|f| format!("{} mode {:04o}", f.path.display(), f.mode & 0o7777)),
        );
    }
    let below = if writable.len() + blocked.len() == 1 {
        "it"
    } else {
        "them"
    };
    Check::new(
        CheckId::DataPath,
        Status::Fail,
        format!(
            "{}, so msb will not open Puddle's data below {below}",
            problems.join("; ")
        ),
    )
    .finding(if blocked.is_empty() {
        Finding::DataPathWritable
    } else {
        Finding::DataPathBlocked
    })
    .fix(fixes.join("\n"))
    .detail(detail.join("\n"))
}

// ---- runtime ----

fn runtime(os: Os, rt: &RuntimeFacts) -> Check {
    let id = CheckId::Runtime;
    let dir = rt.dir.display();
    match &rt.state {
        RuntimeState::Ready {
            version,
            overridden: false,
        } => Check::new(
            id,
            Status::Ok,
            format!("msb {version} ({})", rt.msb.display()),
        ),
        RuntimeState::Ready {
            version,
            overridden: true,
        } => Check::new(
            id,
            Status::Warn,
            format!(
                "msb {version} accepted by the developer override; this build needs {}",
                rt.expected
            ),
        )
        .finding(Finding::RuntimeOverridden)
        .fix("Development builds only. Unset PUDDLE_DEV_ANY_RUNTIME to test the shipped combination."),
        RuntimeState::AccessDenied(denied) => Check::new(
            id,
            Status::Fail,
            format!(
                "you aren't allowed to read or run {}",
                denied.path.display()
            ),
        )
        .finding(Finding::RuntimePermissions)
        .fix(permissions_fix(os, &denied.path))
        .detail(denied.detail.clone()),
        RuntimeState::Unusable(err) => {
            let (finding, fix) = match err {
                RuntimeError::Missing { .. } => (
                    Finding::RuntimeMissing,
                    format!(
                        "Reinstall Puddle: its runtime folder {dir} is incomplete. The runtime ships with Puddle \
and must sit next to Puddle's program file."
                    ),
                ),
                RuntimeError::Mismatch { .. } | RuntimeError::NoVersion { .. } => (
                    Finding::RuntimeVersionMismatch,
                    format!(
                        "Reinstall Puddle so the runtime in {dir} matches this Puddle again. Don't copy an msb \
from elsewhere into it: Puddle only runs the exact msb {} it was built for.",
                        rt.expected
                    ),
                ),
                _ => (
                    Finding::RuntimeUnreadable,
                    format!("Reinstall Puddle: the runtime in {dir} is damaged."),
                ),
            };
            Check::new(id, Status::Fail, err.to_string())
                .finding(finding)
                .fix(fix)
        }
    }
}

// ---- launch and test boot: process outcomes ----

/// Windows error codes and `NTSTATUS` values that say who stopped a process.
mod codes {
    pub(super) const ERROR_ACCESS_DENIED: i32 = 5;
    pub(super) const ERROR_VIRUS_INFECTED: i32 = 225;
    pub(super) const ERROR_VIRUS_DELETED: i32 = 226;
    pub(super) const ERROR_INVALID_IMAGE_HASH: i32 = 577;
    pub(super) const ERROR_ACCESS_DISABLED_BY_POLICY: i32 = 1260;
    /// `ERROR_SYSTEM_INTEGRITY_*`: App Control policy and Smart App Control reputation verdicts.
    pub(super) const SYSTEM_INTEGRITY: [std::ops::RangeInclusive<i32>; 2] =
        [4550..=4562, 4580..=4582];

    pub(super) const STATUS_ACCESS_DENIED: u32 = 0xC000_0022;
    pub(super) const STATUS_DLL_NOT_FOUND: u32 = 0xC000_0135;
    pub(super) const STATUS_ORDINAL_NOT_FOUND: u32 = 0xC000_0138;
    pub(super) const STATUS_ENTRYPOINT_NOT_FOUND: u32 = 0xC000_0139;
    pub(super) const STATUS_INVALID_IMAGE_HASH: u32 = 0xC000_0428;
    pub(super) const STATUS_VIRUS_INFECTED: u32 = 0xC000_0906;

    /// `EACCES` on Linux.
    pub(super) const EACCES: i32 = 13;
}

/// Who refused to start msb, and the fix.
struct Blocked {
    finding: Finding,
    summary: String,
    fix: String,
}

fn applocker(msb: &Path) -> Blocked {
    Blocked {
        finding: Finding::BlockedByAppLocker,
        summary: "an AppLocker or Software Restriction policy blocks msb".into(),
        fix: format!(
            "The policy doesn't allow {}. Allow Puddle's install folder (a path or publisher rule), or \
install Puddle where the policy lets programs run. Event Viewer > Applications and Services \
Logs > Microsoft > Windows > AppLocker > EXE and DLL names the rule.",
            msb.display()
        ),
    }
}

fn app_control(msb: &Path) -> Blocked {
    Blocked {
        finding: Finding::BlockedByAppControl,
        summary: "App Control for Business (WDAC) or Smart App Control blocks msb".into(),
        fix: format!(
            "A code integrity policy doesn't allow {}. Allow Puddle's files or signer in that policy; \
Event Viewer > Applications and Services Logs > Microsoft > Windows > CodeIntegrity > \
Operational names the rule. Smart App Control has no exceptions: it can only be turned off \
(Windows Security > App & browser control > Smart App Control).",
            msb.display()
        ),
    }
}

fn antivirus(msb: &Path) -> Blocked {
    Blocked {
        finding: Finding::BlockedByAntivirus,
        summary: "antivirus flagged msb".into(),
        fix: format!(
            "Look for {} in your antivirus's protection history or quarantine. If it is a false \
positive, restore it, exclude Puddle's runtime folder, and reinstall Puddle if the file is gone.",
            msb.display()
        ),
    }
}

fn edr(msb: &Path) -> Blocked {
    Blocked {
        finding: Finding::ProcessCreationDenied,
        summary: "Windows refused to start msb although its file permissions allow it".into(),
        fix: format!(
            "Endpoint security (EDR) blocking new programs is the usual cause. Check its console or log \
for {}; it must be allowed to run and to start virtual machines through the Windows \
Hypervisor Platform.",
            msb.display()
        ),
    }
}

fn file_permissions(msb: &Path) -> Blocked {
    Blocked {
        finding: Finding::RuntimePermissions,
        summary: "the file's permissions don't let you run msb".into(),
        fix: permissions_fix(Os::Linux, msb),
    }
}

/// How to give this user back the right to read and run `msb`.
fn permissions_fix(os: Os, msb: &Path) -> String {
    if os == Os::Windows {
        format!(
            "The file's permissions deny your account. Restore the inherited permissions:
  icacls \"{}\" /reset
(as administrator if Puddle is installed for all users), or reinstall Puddle.",
            msb.display()
        )
    } else {
        format!(
            "Make it readable and executable again:
  chmod a+rx \"{}\"
or reinstall Puddle.",
            msb.display()
        )
    }
}

/// Classifies a failed `CreateProcess`/`exec`.
fn spawn_failure(os: Os, msb: &Path, os_error: Option<i32>) -> Option<Blocked> {
    let code = os_error?;
    if os == Os::Windows {
        match code {
            codes::ERROR_ACCESS_DISABLED_BY_POLICY => Some(applocker(msb)),
            codes::ERROR_INVALID_IMAGE_HASH => Some(app_control(msb)),
            c if codes::SYSTEM_INTEGRITY.iter().any(|r| r.contains(&c)) => Some(app_control(msb)),
            codes::ERROR_VIRUS_INFECTED | codes::ERROR_VIRUS_DELETED => Some(antivirus(msb)),
            // The runtime check already opened the file for running, so permissions allow it.
            codes::ERROR_ACCESS_DENIED => Some(edr(msb)),
            // Not found (2, 3) and anything else: no one to blame.
            _ => None,
        }
    } else if code == codes::EACCES {
        Some(file_permissions(msb))
    } else {
        None
    }
}

/// Classifies a Windows process that died while loading, by its `NTSTATUS` exit code.
fn load_failure(os: Os, msb: &Path, code: i32, hypervisor_ready: bool) -> Option<Blocked> {
    if os != Os::Windows {
        return None;
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "Windows reports NTSTATUS exit codes as i32; the bits are the code"
    )]
    let status = code as u32;
    match status {
        codes::STATUS_INVALID_IMAGE_HASH => Some(app_control(msb)),
        codes::STATUS_ACCESS_DENIED => Some(edr(msb)),
        codes::STATUS_VIRUS_INFECTED => Some(antivirus(msb)),
        codes::STATUS_DLL_NOT_FOUND
        | codes::STATUS_ORDINAL_NOT_FOUND
        | codes::STATUS_ENTRYPOINT_NOT_FOUND => Some(Blocked {
            finding: Finding::DllMissing,
            summary: format!("msb can't load a DLL it needs (0x{status:08X})"),
            fix: if hypervisor_ready {
                "Reinstall Puddle; if that doesn't help, run Windows Update: msb needs the Windows Hypervisor \
Platform DLLs of a current Windows 10/11."
                    .into()
            } else {
                "msb links the Windows Hypervisor Platform's DLL: fix \"Hypervisor\" above first."
                    .into()
            },
        }),
        _ => None,
    }
}

fn hex_or_dec(os: Os, code: i32) -> String {
    if os == Os::Windows && code < 0 {
        #[expect(
            clippy::cast_sign_loss,
            reason = "Windows reports NTSTATUS exit codes as i32; the bits are the code"
        )]
        let status = code as u32;
        format!("0x{status:08X}")
    } else {
        code.to_string()
    }
}

fn blocked_check(id: CheckId, b: Blocked, detail: String) -> Check {
    Check::new(id, Status::Fail, b.summary)
        .finding(b.finding)
        .fix(b.fix)
        .detail(detail)
}

fn launch(os: Os, msb: &Path, outcome: &ProcessOutcome, hypervisor_ready: bool) -> Check {
    let id = CheckId::Launch;
    match outcome {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line,
            elapsed,
            ..
        } => Check::new(
            id,
            Status::Ok,
            format!(
                "{} ({})",
                if stdout_first_line.is_empty() {
                    "msb answered"
                } else {
                    stdout_first_line
                },
                seconds(*elapsed)
            ),
        ),
        ProcessOutcome::SpawnFailed { os_error, detail } => {
            match spawn_failure(os, msb, *os_error) {
                Some(b) => blocked_check(id, b, detail.clone()),
                None => Check::new(id, Status::Fail, "msb could not be started")
                    .finding(Finding::LaunchFailed)
                    .fix("Reinstall Puddle. If this stays, include the line below when you report it.")
                    .detail(detail.clone()),
            }
        }
        ProcessOutcome::Exited {
            code, stderr_tail, ..
        } => {
            if let Some(b) = code.and_then(|c| load_failure(os, msb, c, hypervisor_ready)) {
                return blocked_check(id, b, stderr_tail.clone());
            }
            let how = code.map_or_else(
                || "was killed by a signal".to_owned(),
                |c| format!("exited with {}", hex_or_dec(os, c)),
            );
            Check::new(id, Status::Fail, format!("msb {how} instead of answering"))
                .finding(Finding::LaunchExited)
                .fix(STOPPED_FIX)
                .detail(stderr_tail.clone())
        }
        ProcessOutcome::TimedOut { after } => Check::new(
            id,
            Status::Fail,
            format!("msb didn't answer within {}", seconds(*after)),
        )
        .finding(Finding::LaunchTimedOut)
        .fix(SLOW_FIX),
    }
}

const STOPPED_FIX: &str = "If endpoint security (EDR or antivirus) runs on this PC, check its log for msb: it may have \
stopped the process. Otherwise reinstall Puddle and include the lines below when you report it.";

const SLOW_FIX: &str = "Security software scanning a program it hasn't seen before is the usual cause. \
Run puddle doctor again; if it keeps timing out, check your endpoint security's log for msb.";

fn test_boot(os: Os, msb: &Path, dir: &Path, facts: &BootFacts, job: Option<&JobFacts>) -> Check {
    let check = test_boot_outcome(os, msb, dir, facts, job);
    match facts {
        BootFacts::Ran {
            kept_logs: Some(kept),
            ..
        } => with_kept_logs(check, kept),
        _ => check,
    }
}

/// Adds where msb's logs of the failed boot went, or why they didn't, to the check's evidence: the
/// logs were asked for, so a failure to keep them is said, not swallowed.
fn with_kept_logs(mut check: Check, kept: &KeptLogs) -> Check {
    let line = match kept {
        KeptLogs::Kept { dir, failed } if failed.is_empty() => {
            format!("msb's logs of this boot are kept in {}", dir.display())
        }
        KeptLogs::Kept { dir, failed } => format!(
            "msb's logs of this boot are kept in {}, except {}",
            dir.display(),
            failed.join("; ")
        ),
        KeptLogs::NotKept { detail } => format!("msb's logs of this boot were not kept: {detail}"),
    };
    let detail = match check.detail.take() {
        Some(detail) => format!("{detail}\n{line}"),
        None => line,
    };
    check.detail = Some(detail);
    check
}

fn test_boot_outcome(
    os: Os,
    msb: &Path,
    dir: &Path,
    facts: &BootFacts,
    job: Option<&JobFacts>,
) -> Check {
    let id = CheckId::TestBoot;
    let outcome = match facts {
        BootFacts::UnsupportedArch { arch } => {
            return skipped(id, &format!("no test program for {arch}"));
        }
        BootFacts::SetupFailed { detail } => {
            return Check::new(id, Status::Fail, "couldn't prepare the test VM's files")
                .finding(Finding::BootSetupFailed)
                .fix("Check that your temp folder (TEMP) exists, is writable and has free space.")
                .detail(detail.clone());
        }
        BootFacts::Ran { outcome, .. } => outcome,
    };
    match outcome {
        ProcessOutcome::Exited {
            code: Some(PROBE_EXIT_CODE),
            elapsed,
            ..
        } => Check::new(
            id,
            Status::Ok,
            format!(
                "a test VM booted and ran a program in {}",
                seconds(*elapsed)
            ),
        ),
        ProcessOutcome::SpawnFailed { os_error, detail } => {
            match spawn_failure(os, msb, *os_error) {
                Some(b) => blocked_check(id, b, detail.clone()),
                None => Check::new(id, Status::Fail, "msb could not be started")
                    .finding(Finding::BootFailed)
                    .fix(STOPPED_FIX)
                    .detail(detail.clone()),
            }
        }
        ProcessOutcome::TimedOut { after } => Check::new(
            id,
            Status::Fail,
            format!("the test VM didn't finish within {}", seconds(*after)),
        )
        .finding(Finding::BootTimedOut)
        .fix(SLOW_FIX),
        ProcessOutcome::Exited {
            code, stderr_tail, ..
        } => {
            let denied =
                stderr_tail.contains("Access is denied") || stderr_tail.contains("os error 5)");
            let restrictive_job = job.is_some_and(JobFacts::forbids_breakaway);
            let how = code.map_or_else(
                || "was killed by a signal".to_owned(),
                |c| format!("exited with {}", hex_or_dec(os, c)),
            );
            if os == Os::Windows && denied && restrictive_job {
                return Check::new(
                    id,
                    Status::Fail,
                    format!("msb {how}: access denied starting the VM process inside this job object"),
                )
                .finding(Finding::BootBlockedByJob)
                .fix(
                    "The job object Puddle was started in (see \"Job object\") doesn't let the VM process leave \
it. Run puddle doctor from a normal terminal; on a CI runner, start it through a process \
outside the step's job.",
                )
                .detail(stderr_tail.clone());
            }
            if os == Os::Windows && denied {
                return blocked_check(id, edr(msb), stderr_tail.clone());
            }
            if stderr_tail.contains(RUNTIME_INCOMPLETE) {
                return Check::new(
                    id,
                    Status::Fail,
                    "msb can't find its firmware library (libkrunfw) beside it",
                )
                .finding(Finding::RuntimeMissing)
                .fix(format!(
                    "Reinstall Puddle: its runtime folder {} is incomplete. msb needs the libkrunfw \
it was released with in the same folder.",
                    dir.display()
                ))
                .detail(stderr_tail.clone());
            }
            Check::new(
                id,
                Status::Fail,
                format!("msb {how} without booting the test VM"),
            )
            .finding(Finding::BootFailed)
            .fix(BOOT_FIX)
            .detail(stderr_tail.clone())
        }
    }
}

/// msb's error when the firmware library it pairs with isn't beside it.
const RUNTIME_INCOMPLETE: &str = "runtime installation is incomplete";

const BOOT_FIX: &str = "msb starts but can't boot a VM. If endpoint security runs on this PC, check its log for msb: \
it must be allowed to start virtual machines through the hypervisor. Otherwise restart the PC \
and run puddle doctor again, and include the lines below when you report it.";

// ---- SSH agent ----

const SSH_AGENT_LIMITS: &str = "SSH to servers other than Git hosts gets no key from Puddle";

/// `n` keys of `families` as `ed25519: 1, rsa: 1`: families in alphabetical order, never a key.
fn family_counts(families: &[String]) -> String {
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for family in families {
        *counts.entry(family.as_str()).or_default() += 1;
    }
    counts
        .iter()
        .map(|(family, n)| format!("{family}: {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn ssh_agent_missing_fix(os: Os, service: Option<AgentService>) -> String {
    let start = match service {
        Some(AgentService::Disabled) => {
            "The OpenSSH Authentication Agent service is disabled: set its startup type to Manual or Automatic \
in Services (services.msc), then start it."
        }
        Some(AgentService::Stopped) => {
            "The OpenSSH Authentication Agent service is not running: start it in Services (services.msc), or in \
PowerShell as administrator: Start-Service ssh-agent."
        }
        Some(AgentService::Missing) => {
            "The OpenSSH Authentication Agent service is not installed: add it under Settings > System > Optional \
features > OpenSSH Client, or use 1Password or Bitwarden, which serve the same pipe."
        }
        _ if os == Os::Windows => {
            "Start your agent (the OpenSSH Authentication Agent service, 1Password or Bitwarden) or name its pipe \
in the settings (`ui.ssh_agent_pipe`; for Pageant its OpenSSH pipe)."
        }
        _ => {
            "Start your agent (ssh-agent, gpg-agent, 1Password or Bitwarden) in the session that starts Puddle so \
that SSH_AUTH_SOCK is set, or name its socket in the settings (`ui.ssh_agent_pipe`)."
        }
    };
    format!(
        "{SSH_AGENT_LIMITS}. {start} Git hosts need no agent: Puddle ends their SSH sessions itself, so Git over SSH is not \
affected."
    )
}

// ---- Azure CLI ----

/// Warn only: the Azure CLI is optional, needed for the Azure DevOps sources and nothing else.
fn az_cli(facts: &AzCliFacts) -> Check {
    let id = CheckId::AzCli;
    match facts {
        AzCliFacts::NotFound => Check::new(
            id,
            Status::Warn,
            "not found; needed only for Azure DevOps through `az`",
        )
        .finding(Finding::AzCliMissing)
        .fix("Install the Azure CLI (`az`) only if you use Azure DevOps sources, then run `az login --tenant <tenant>` in a terminal."),
        AzCliFacts::Found { version } => Check::new(id, Status::Ok, format!("version {version}")),
        AzCliFacts::Unknown { detail } => Check::new(
            id,
            Status::Warn,
            format!("found, but its version could not be read: {detail}"),
        )
        .fix("Azure DevOps sources need a working `az`; reinstall the Azure CLI if `az --version` fails."),
    }
}

fn ssh_agent(os: Os, facts: &SshAgentFacts) -> Check {
    let id = CheckId::SshAgent;
    match facts {
        SshAgentFacts::NoEndpoint => Check::new(
            id,
            Status::Info,
            "no agent is named: SSH_AUTH_SOCK is not set in Puddle's session",
        )
        .finding(Finding::SshAgentMissing)
        .fix(ssh_agent_missing_fix(os, None)),
        SshAgentFacts::Unreachable {
            endpoint,
            detail,
            service,
            by_default,
        } => Check::new(
            id,
            if *by_default {
                Status::Info
            } else {
                Status::Warn
            },
            format!(
                "no agent answers on {endpoint}: {SSH_AGENT_LIMITS}; start your agent or name its {} in the settings",
                if os == Os::Windows { "pipe" } else { "socket" }
            ),
        )
        .finding(Finding::SshAgentMissing)
        .fix(ssh_agent_missing_fix(os, *service))
        .detail(detail.clone()),
        SshAgentFacts::Locked { endpoint } => Check::new(
            id,
            Status::Warn,
            format!("the agent on {endpoint} is locked"),
        )
        .finding(Finding::SshAgentLocked)
        .fix(format!(
            "{SSH_AGENT_LIMITS} while the agent is locked. Unlock it (1Password, Bitwarden) or run ssh-add to \
add a key. Git hosts need no agent."
        )),
        SshAgentFacts::TimedOut { endpoint, limit } => Check::new(
            id,
            Status::Info,
            format!(
                "the agent on {endpoint} did not answer within {} s; it may be waiting for you to unlock it",
                limit.as_secs()
            ),
        )
        .finding(Finding::SshAgentSlow)
        .fix(
            "Agents that ask you before they answer (1Password, Bitwarden) do this while they are locked. Unlock the agent \
and run the check again. Git hosts need no agent.",
        ),
        SshAgentFacts::Unusable { endpoint, detail } => Check::new(
            id,
            Status::Info,
            format!("what answers on {endpoint} does not speak the SSH agent protocol"),
        )
        .finding(Finding::SshAgentUnusable)
        .fix(format!(
            "{SSH_AGENT_LIMITS}. Check that the setting `ui.ssh_agent_pipe` (or SSH_AUTH_SOCK) names an SSH agent. \
Git hosts need no agent."
        ))
        .detail(detail.clone()),
        SshAgentFacts::Keys {
            endpoint,
            families,
        } if families.is_empty() => Check::new(
            id,
            Status::Info,
            format!("the agent on {endpoint} holds no keys"),
        )
        .finding(Finding::SshAgentEmpty)
        .fix(format!(
            "{SSH_AGENT_LIMITS}. Add a key to the agent (ssh-add, or in 1Password or Bitwarden). Git hosts need no agent."
        )),
        SshAgentFacts::Keys {
            endpoint,
            families,
        } => Check::new(
            id,
            Status::Ok,
            format!(
                "the agent on {endpoint} holds {} ({})",
                match families.len() {
                    1 => "1 key".to_owned(),
                    n => format!("{n} keys"),
                },
                family_counts(families)
            ),
        ),
    }
}

// ---- Global Secure Access ----

/// The Windows runtime's guest kernel is built with `/dev/mem` (the Linux runtime's is not), so a
/// root process in a sandbox can map the virtual devices' registers past the guest kernel.
fn windows_guest_kernel() -> Check {
    Check::new(
        CheckId::GuestKernel,
        Status::Info,
        "the guest kernel of the Windows runtime has /dev/mem",
    )
    .finding(Finding::GuestDevMemPresent)
    .fix(
        "Puddle works. Root in a sandbox can map the virtual devices' registers through /dev/mem, \
so the runtime's own checks on every register access are the only line there. The Linux runtime's \
guest kernel is built without /dev/mem.",
    )
}

fn global_secure_access(gsa: &GsaFacts) -> Check {
    let id = CheckId::GlobalSecureAccess;
    match gsa {
        GsaFacts::NotInstalled => Check::new(id, Status::Ok, "client not installed"),
        GsaFacts::Unknown { detail } => Check::new(
            id,
            Status::Info,
            "could not check whether the client is installed",
        )
        .finding(Finding::GlobalSecureAccessUnknown)
        .fix(
            "Puddle works without it. If company resources reached through Microsoft Entra Global Secure \
Access don't work from a sandbox, see whether its client is installed on this PC: it doesn't \
tunnel virtual machines' traffic.",
        )
        .detail(detail.clone()),
        GsaFacts::Installed { running } => Check::new(
            id,
            Status::Warn,
            format!(
                "Microsoft Entra Global Secure Access client installed ({})",
                match running {
                    Some(true) => "running",
                    Some(false) => "not running",
                    None => "running state unknown",
                }
            ),
        )
        .finding(Finding::GlobalSecureAccessInstalled)
        .fix(
            "Microsoft documents that this client doesn't tunnel virtual machines' traffic and isn't \
supported on devices that host VMs. Sandboxes have no network of their own: their traffic \
leaves through Puddle's proxy, an ordinary program on this PC, which the client handles like \
any other app. If company resources reached through Global Secure Access don't work from a \
sandbox, this limitation is the likely cause; Puddle can't change it.",
        ),
    }
}

fn seconds(d: Duration) -> String {
    let ms = d.as_millis();
    format!("{}.{} s", ms / 1000, ms % 1000 / 100)
}

#[cfg(test)]
mod tests;
