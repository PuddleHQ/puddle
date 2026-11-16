// SPDX-License-Identifier: AGPL-3.0-or-later
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

#[path = "fake_host/attach_places.rs"]
mod attach_places;
#[path = "fake_host/cleanup_and_problems.rs"]
mod cleanup_and_problems;
#[path = "fake_host/credential_store_and_shell_init.rs"]
mod credential_store_and_shell_init;
#[path = "fake_host/delete_rename_activity.rs"]
mod delete_rename_activity;
#[path = "fake_host/direct_ssh_and_logins.rs"]
mod direct_ssh_and_logins;
#[path = "fake_host/dropped_requests.rs"]
mod dropped_requests;
#[path = "fake_host/editor_attach.rs"]
mod editor_attach;
#[path = "fake_host/editor_problems.rs"]
mod editor_problems;
#[path = "fake_host/environment_and_secrets.rs"]
mod environment_and_secrets;
#[path = "fake_host/forwards_and_local_checkout.rs"]
mod forwards_and_local_checkout;
#[path = "fake_host/front.rs"]
mod front;
#[path = "fake_host/grow_room.rs"]
mod grow_room;
#[path = "fake_host/guest_boot_plan.rs"]
mod guest_boot_plan;
#[path = "fake_host/guest_problems_and_variables.rs"]
mod guest_problems_and_variables;
#[path = "fake_host/lifecycle_refusals.rs"]
mod lifecycle_refusals;
#[path = "fake_host/live_settings_changes.rs"]
mod live_settings_changes;
#[path = "fake_host/missing_volume_and_ids.rs"]
mod missing_volume_and_ids;
#[path = "fake_host/proxy_sign_ins_and_audit.rs"]
mod proxy_sign_ins_and_audit;
#[path = "fake_host/repositories.rs"]
mod repositories;
#[path = "fake_host/restart_and_crash.rs"]
mod restart_and_crash;
#[path = "fake_host/restore.rs"]
mod restore;
#[path = "fake_host/root_disk.rs"]
mod root_disk;
#[path = "fake_host/sandbox_resources.rs"]
mod sandbox_resources;
#[path = "fake_host/shutdown.rs"]
mod shutdown;
#[path = "fake_host/start_failures_and_trust.rs"]
mod start_failures_and_trust;
#[path = "fake_host/start_folder_and_dev_cert.rs"]
mod start_folder_and_dev_cert;
#[path = "fake_host/startup_and_api.rs"]
mod startup_and_api;
#[path = "fake_host/terminals_and_mise.rs"]
mod terminals_and_mise;
#[path = "fake_host/tools_edit.rs"]
mod tools_edit;
#[path = "fake_host/tools_install.rs"]
mod tools_install;
#[path = "fake_host/workspace_list_at_start.rs"]
mod workspace_list_at_start;

use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_api::WorkspaceService as _;
use puddle_api::{
    BrowserTarget, DesktopTarget, LaunchError, Launcher, UiAssets, UiFile, WorkspaceRecord,
};
use puddle_certs::{CorporateRoots, SOURCES, StoreSnapshot};
use puddle_compute::fake::{ExecContext, FakeRuntime, Fault, Op};
use puddle_compute::{ExecOutput, ExecRequest, ImageConfig, Runtime};
use puddle_host::{
    DevCertSource, FreeSpaceTiming, GuestSettings, Host, HostConfig, HostError, HostPaths,
    PREPARE_STEPS, Platform, RewriteGate, RuntimeFactory, RuntimeInputs, SHUTDOWN_STEPS,
    START_STEPS, Step, prepare,
};
use puddle_netpolicy::EndpointKind;
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::{MemoryStore, SecretStore as _};
use puddle_test_kit::{eventually, next_event_matching, no_event_matching};
use puddle_types::fixture;
use puddle_types::{
    DiskSpaceLevel, Event, EventSink, GuestEnv, GuestPath, ImageRef, SandboxName, WorkspaceId,
};
use puddle_upstream::{Discovery, FakeOs, Mode, ProxyConfig};
use puddle_workspace::{CLEAR_LOCKS_SH, DELETE_CHECK_SH, FINISH_IMPORT_SH, PREPARE_SH};
use rustls::pki_types::pem::PemObject;
use serde_json::json;
use support::{Api, TestDevCert, ended, hermetic_options, named_id_options};

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
            return Err(HostError::Roots(
                puddle_certs::StoreError::DisallowedUnreadable {
                    store: SOURCES[0],
                    reason: format!("{what} refused"),
                },
            ));
        }
        Ok(())
    }
}

impl Platform for FakePlatform {
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
        assert!(inputs.guest_share.starts_with(&inputs.data));
        std::future::ready(Ok(self.runtime.clone()))
    }
}

/// The scripted guest: answers every command Puddle runs in a sandbox.
#[derive(Default)]
struct Guest {
    commands: Mutex<Vec<String>>,
    /// Who each command ran as, `<kind> <user>` (`-` when it named none), in order.
    users: Mutex<Vec<String>>,
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
    /// What `puddle-agent mise-fetch` answers on standard error with exit code 1, instead of
    /// success.
    mise_failure: Mutex<Option<&'static str>>,
    /// Every `puddle-agent mise-fetch` as (arguments, user).
    mise_fetches: Mutex<Vec<(Vec<String>, Option<String>)>>,
    /// The tools `puddle-agent mise-install` finds missing; a good install clears them.
    tools_missing: Mutex<Vec<String>>,
    /// What an install (not a report) answers on standard error with exit code 1, leaving the
    /// tools missing, instead of installing them.
    tools_failure: Mutex<Option<&'static str>>,
    /// Runs inside every install (not a report), before it answers: what the tools asked of the
    /// network while it ran.
    at_install: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Every `puddle-agent mise-install` as (arguments, user), reports included.
    tools_runs: Mutex<Vec<(Vec<String>, Option<String>)>>,
    /// A report (`--missing`) answers with lines that are not the agent's.
    tools_report_garbled: AtomicBool,
    /// The root `mise.toml` as `puddle-agent mise-tools` sees it: its tools and how often it
    /// changed (the hash is made from the count; 0 is no file).
    mise_root: Mutex<(std::collections::BTreeMap<String, String>, u64)>,
    /// What every `mise-tools` call answers on standard error with exit code 1, instead of its
    /// answer.
    mise_tools_failure: Mutex<Option<&'static str>>,
    /// `mise-tools list` prints lines that are not the agent's.
    mise_tools_garbled: AtomicBool,
    /// Every `puddle-agent mise-tools` as (arguments after the workspace folder, user).
    mise_tools_runs: Mutex<Vec<(Vec<String>, Option<String>)>>,
    /// How often the trust step for a new JDK ran, and as whom.
    java_trusts: Mutex<Vec<Option<String>>>,
    /// The trust step for a new JDK fails.
    java_trust_fails: AtomicBool,
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
    /// Each import's arguments, with what the guest read at its mounted source's `HEAD`,
    /// whether a file beside the `.git` (the working tree) could be read there, and the hook
    /// setting its environment gave git.
    imports: Mutex<Vec<SeenImport>>,
    /// Every import fails the way git does for a repository it can't read.
    fail_import: AtomicBool,
    /// The listing script fails the way it does for a volume it cannot enter.
    fail_listing: AtomicBool,
    /// The listing script finds no git in the image (it exits 4 and says so).
    git_missing: AtomicBool,
    /// Checkouts the listing reports as unreadable, by directory and message.
    unreadable: Mutex<Vec<(String, String)>>,
    /// The next listing waits here before it answers, and says so in `listing_waits`.
    listing_hold: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    listing_waits: AtomicBool,
}

/// One import as the fake guest saw it: its arguments, its source's `HEAD`, whether the working
/// tree was readable, and the hook setting its environment gave git.
type SeenImport = (Vec<String>, String, bool, Option<String>);

impl Guest {
    fn install(self: &Arc<Self>, runtime: &FakeRuntime) {
        let guest = self.clone();
        runtime.on_exec(move |ctx: &mut ExecContext<'_>, r: &ExecRequest| guest.answer(ctx, r));
    }

    fn answer(&self, ctx: &ExecContext<'_>, r: &ExecRequest) -> Option<ExecOutput> {
        let second = r.args.get(1).map(String::as_str);
        if let Some(answer) = self
            .answer_volume_question(r)
            .or_else(|| self.answer_tools(r))
        {
            return Some(answer);
        }
        let kind = match (r.program.as_str(), r.args.first().map(String::as_str)) {
            ("/bin/sh", Some("/puddle/boot.sh")) => "boot",
            ("sh", _) if second == Some(PREPARE_SH) => "layout",
            ("sh", _) if r.args.get(2).map(String::as_str) == Some("puddle-start-folder") => {
                "start-folder"
            }
            ("sh", _) if second == Some(CLEAR_LOCKS_SH) => "locks",
            ("sh", _) if second == Some(FINISH_IMPORT_SH) => "finish-import",
            ("git", _) if r.args.iter().any(|a| a == "--no-local") => "import",
            ("sh", _) if second == Some(DELETE_CHECK_SH) => "check",
            ("sh", _) if second.is_some_and(|s| s.contains("fstrim")) => "shutdown-trim",
            ("sh", _) if second.is_some_and(|s| s.contains("mktemp")) => "shell-init-check",
            ("fstrim", _) => "trim",
            ("git", _) => "clone",
            ("sync", _) => "sync",
            _ => return None,
        };
        self.record(ctx, kind, r);
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
            "import" => self.answer_import(ctx, r),
            "finish-import" => {
                let (dest, url) = (&r.args[3], &r.args[5]);
                let dir = dest.rsplit('/').next().unwrap_or_default().to_owned();
                self.volume.lock().unwrap().push((dir, url.clone()));
                ExecOutput::new(0, "", "")
            }
            "shell-init-check" => self.answer_shell_init_check(ctx, r),
            "layout" if self.fail_layout.load(Ordering::SeqCst) => ExecOutput::new(
                1,
                "",
                "cannot create /workspaces/x/.puddle: read-only file system",
            ),
            "layout" => ExecOutput::new(0, "D\n", ""),
            "locks" => match *self.locks_answer.lock().unwrap() {
                Some((code, out)) => ExecOutput::new(code, out, ""),
                None => ExecOutput::new(0, "D\n", ""),
            },
            "trim" | "shutdown-trim" if self.fail_trim.load(Ordering::SeqCst) => {
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

    /// The managed tools commands: the mise fetch, the install and the trust step for a new JDK.
    /// They run in the background after a start, so their place among the commands a test counts
    /// is a race: they stay out of the command log and have logs of their own.
    fn answer_tools(&self, r: &ExecRequest) -> Option<ExecOutput> {
        Some(
            match (r.program.as_str(), r.args.first().map(String::as_str)) {
                ("/puddle/puddle-agent", Some("mise-fetch")) => self.answer_mise_fetch(r),
                ("/puddle/puddle-agent", Some("mise-install")) => self.answer_mise_install(r),
                ("/puddle/puddle-agent", Some("mise-tools")) => self.answer_mise_tools(r),
                ("sh", Some(puddle_certs::JAVA_STEP_PATH)) => {
                    self.java_trusts.lock().unwrap().push(r.user.clone());
                    if self.java_trust_fails.load(Ordering::SeqCst) {
                        ExecOutput::new(1, "", "keytool: permission denied")
                    } else {
                        ExecOutput::new(0, "java-trust: 1 store updated\n", "")
                    }
                }
                _ => return None,
            },
        )
    }

    /// Records a `mise-fetch` and answers as set up.
    fn answer_mise_fetch(&self, r: &ExecRequest) -> ExecOutput {
        self.mise_fetches
            .lock()
            .unwrap()
            .push((r.args.clone(), r.user.clone()));
        match *self.mise_failure.lock().unwrap() {
            Some(said) => ExecOutput::new(1, "", said),
            None => ExecOutput::new(0, "installed\n", ""),
        }
    }

    /// Records a `mise-install` and answers like the agent: the missing tools, and for an install
    /// what landed or failed, then `end`.
    fn answer_mise_install(&self, r: &ExecRequest) -> ExecOutput {
        self.tools_runs
            .lock()
            .unwrap()
            .push((r.args.clone(), r.user.clone()));
        let report_only = r.args.get(2).map(String::as_str) == Some("--missing");
        if report_only && self.tools_report_garbled.load(Ordering::SeqCst) {
            return ExecOutput::new(0, "Segmentation fault\n", "");
        }
        let before = self.tools_missing.lock().unwrap().clone();
        let mut out = String::new();
        for tool in &before {
            writeln!(out, "missing {tool}").unwrap();
        }
        if report_only || before.is_empty() {
            out.push_str("end\n");
            return ExecOutput::new(0, out, "");
        }
        if let Some(during) = self.at_install.lock().unwrap().as_ref() {
            during();
        }
        let failure = *self.tools_failure.lock().unwrap();
        let word = if failure.is_some() {
            "failed"
        } else {
            "installed"
        };
        for tool in &before {
            writeln!(out, "{word} {tool}").unwrap();
        }
        out.push_str("end\n");
        if let Some(said) = failure {
            return ExecOutput::new(1, out, said);
        }
        self.tools_missing.lock().unwrap().clear();
        ExecOutput::new(0, out, "")
    }

    /// Answers `mise-tools` like the agent over a stand-in root file: a listing, an edit that
    /// refuses a stale hash with exit code 5, the registry and the versions.
    fn answer_mise_tools(&self, r: &ExecRequest) -> ExecOutput {
        let words: Vec<String> = r.args.iter().skip(2).cloned().collect();
        self.mise_tools_runs
            .lock()
            .unwrap()
            .push((words.clone(), r.user.clone()));
        if let Some(said) = *self.mise_tools_failure.lock().unwrap() {
            return ExecOutput::new(1, "", said);
        }
        let mut file = self.mise_root.lock().unwrap();
        let hash = |revision: u64| {
            if revision == 0 {
                "absent".to_owned()
            } else {
                format!("{revision:064x}")
            }
        };
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["list"] if self.mise_tools_garbled.load(Ordering::SeqCst) => {
                ExecOutput::new(0, "Segmentation fault\n", "")
            }
            ["list"] => {
                let mut out = format!("hash\t{}\n", hash(file.1));
                for (name, version) in &file.0 {
                    writeln!(out, "tool\tmise.toml\t{name}\t{version}\t0").unwrap();
                }
                out.push_str("end\n");
                ExecOutput::new(0, out, "")
            }
            ["set" | "remove", ..] if words.last().copied() != Some(hash(file.1).as_str()) => {
                ExecOutput::new(5, "", "mise.toml changed since it was listed")
            }
            ["set", name, version, _] => {
                file.0.insert((*name).to_owned(), (*version).to_owned());
                file.1 += 1;
                ExecOutput::new(0, format!("hash\t{}\nend\n", hash(file.1)), "")
            }
            ["remove", name, _] => {
                file.0.remove(*name);
                file.1 += 1;
                ExecOutput::new(0, format!("hash\t{}\nend\n", hash(file.1)), "")
            }
            ["registry"] => ExecOutput::new(0, "reg\tnode\tJavaScript runtime\nend\n", ""),
            ["versions", _] => ExecOutput::new(0, "ver\t22.1.0\nver\t22.0.0\nend\n", ""),
            other => ExecOutput::new(2, "", format!("unexpected {other:?}")),
        }
    }

    /// The installs (not the reports) `mise-install` ran.
    fn tool_installs(&self) -> usize {
        self.tools_runs
            .lock()
            .unwrap()
            .iter()
            .filter(|(args, _)| args.len() == 2)
            .count()
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

    /// Records an import: what its mounted source's `HEAD` reads, and whether the working tree
    /// next to the `.git` is there to read (it must not be).
    fn answer_import(&self, ctx: &ExecContext<'_>, r: &ExecRequest) -> ExecOutput {
        let src = &r.args[r.args.len() - 2];
        let read = |p: &str| ctx.read(&GuestPath::new(p).unwrap()).ok();
        let head = read(&format!("{src}/.git/HEAD"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let tree_visible = read(&format!("{src}/.env")).is_some();
        let hooks = (0..8).find_map(|i| {
            (ctx.env().get(&format!("GIT_CONFIG_KEY_{i}")) == Some("core.hooksPath"))
                .then(|| {
                    ctx.env()
                        .get(&format!("GIT_CONFIG_VALUE_{i}"))
                        .map(str::to_owned)
                })
                .flatten()
        });
        self.imports
            .lock()
            .unwrap()
            .push((r.args.clone(), head, tree_visible, hooks));
        if self.fail_import.load(Ordering::SeqCst) {
            return ExecOutput::new(128, "", "fatal: bad object HEAD\n");
        }
        ExecOutput::new(0, "", "")
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
            Some("puddle-checkouts") if r.args.get(4).map(String::as_str) == Some("--names") => {
                Some(self.list_volume_names())
            }
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
        if self.git_missing.load(Ordering::SeqCst) {
            return ExecOutput::new(4, "E\t.\tgit is not installed in the image\n", "");
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

    /// What the listing script prints with `--names`: the checkout directories alone, from any
    /// image (it runs no git).
    fn list_volume_names(&self) -> ExecOutput {
        let mut dirs: Vec<String> = self
            .volume
            .lock()
            .unwrap()
            .iter()
            .map(|(d, _)| d.clone())
            .collect();
        dirs.sort();
        let mut out = String::new();
        for dir in dirs {
            writeln!(out, "C\t{dir}\t\t\t\t0").unwrap();
        }
        out.push_str("O\t0\nD\n");
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

    /// Notes that `r` ran in `ctx`'s sandbox as a command of `kind`, and as whom.
    fn record(&self, ctx: &ExecContext<'_>, kind: &str, r: &ExecRequest) {
        self.commands
            .lock()
            .unwrap()
            .push(format!("{} {kind}", ctx.sandbox()));
        self.users
            .lock()
            .unwrap()
            .push(format!("{kind} {}", r.user.as_deref().unwrap_or("-")));
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().unwrap().clone()
    }

    /// The users the commands of `kind` ran as, in order.
    fn users_of(&self, kind: &str) -> Vec<String> {
        self.users
            .lock()
            .unwrap()
            .iter()
            .filter_map(|u| u.strip_prefix(kind)?.strip_prefix(' ').map(str::to_owned))
            .collect()
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

    /// The merge specs of boot plan number `n` (from 0), by guest path.
    fn plan_merges(&self, n: usize) -> std::collections::BTreeMap<String, Vec<u8>> {
        plan_calls(&self.plans.lock().unwrap()[n].1, "merge")
    }

    /// The host keys block boot plan number `n` merges into `ssh_known_hosts`, if it does.
    fn known_hosts_block(&self, n: usize) -> Option<String> {
        let merges = self.plan_merges(n);
        let spec = merges.get("/etc/ssh/ssh_known_hosts")?;
        let spec: serde_json::Value = serde_json::from_slice(spec).unwrap();
        assert_eq!(spec["format"], "known_hosts");
        Some(spec["entries"][0]["value"].as_str().unwrap().to_owned())
    }

    fn boots(&self) -> usize {
        self.plans.lock().unwrap().len()
    }

    /// Waits until the hook has run `n` times in all.
    async fn boots_reach(&self, n: usize) {
        eventually(
            &format!("the boot hook ran {n} times"),
            Duration::from_secs(10),
            || self.boots() >= n,
        )
        .await;
    }
}

/// The files a rendered boot plan writes: each `puddle_file '<path>' <mode> '<printf format>'`
/// line, with the format decoded the way `printf` reads it (`\NNN` is an octal byte).
fn plan_files(plan: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    plan_calls(plan, "file")
}

/// The same for the lines of one call: `file` (a file's contents) or `merge` (a merge spec).
fn plan_calls(plan: &[u8], call: &str) -> std::collections::BTreeMap<String, Vec<u8>> {
    let text = String::from_utf8(plan.to_vec()).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for line in text.split("\npuddle_") {
        let Some(rest) = line
            .strip_prefix(call)
            .and_then(|rest| rest.strip_prefix(' '))
        else {
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
    /// The ids the shell says it keeps browser data under.
    data_under: Mutex<Vec<String>>,
    /// A path to make unwritable when a window is asked to close: the first thing a delete does
    /// once its mark is saved.
    unwritable_on_close: Mutex<Option<PathBuf>>,
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
        workspace: &'a WorkspaceId,
    ) -> futures_util::future::BoxFuture<'a, ()> {
        Box::pin(async move {
            self.closed.lock().unwrap().push(workspace.to_string());
            if let Some(path) = self.unwritable_on_close.lock().unwrap().take() {
                make_unwritable(&path);
            }
        })
    }

    fn forget_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceId,
    ) -> futures_util::future::BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            self.forgotten.lock().unwrap().push(workspace.to_string());
            match self.forget_fails_with.lock().unwrap().clone() {
                Some(message) => Err(LaunchError::new(message)),
                None => Ok(()),
            }
        })
    }

    fn has_browser_data(&self, workspace: &WorkspaceId) -> bool {
        self.data_under
            .lock()
            .unwrap()
            .iter()
            .any(|id| id == workspace.as_str())
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
    /// Starts the host with SSH host keys for Git over SSH.
    git_ssh: AtomicBool,
    /// Where new ids come from, for a test about them; else random ids, as on a real host.
    ids: Mutex<Option<Arc<dyn puddle_api::IdSource>>>,
    /// Whether the managed tools layer is on. Off by default: its background download runs a
    /// command at a moment no test chose, which would move the count of commands that a test
    /// injects a fault after.
    mise_on: AtomicBool,
    /// Starts the host the way `puddle serve --headless` does: no store handed in, so the choice
    /// of store runs and picks the file vault, whose key is a file in this rig's config folder.
    file_vault: AtomicBool,
    /// With `file_vault`: leaves the headless flag off, as a restart of a recorded install does.
    without_headless_flag: AtomicBool,
    /// The drive the host reads its free space from, when a test sets one; else a fixed figure.
    free_space: Mutex<Option<Arc<dyn puddle_host::Space>>>,
}

impl Rig {
    /// A rig whose workspaces get ids made from their names (`acme` gets `acme`), for a test that
    /// writes those ids into paths and URLs itself.
    fn named_ids() -> Self {
        let rig = Self::new();
        *rig.ids.lock().unwrap() = Some(Arc::new(puddle_api::NameIds));
        rig
    }

    fn new() -> Self {
        let runtime = FakeRuntime::new();
        let guest = Arc::new(Guest::default());
        guest.install(&runtime);
        let rig = Self {
            dev_cert: Arc::new(TestDevCert::new()),
            dir: support::scratch_dir(),
            log: Log::default(),
            runtime,
            guest,
            launcher: Arc::new(RecordingLauncher::default()),
            windowless: AtomicBool::new(false),
            secrets: Arc::new(MemoryStore::new()),
            rewrite_gate: Mutex::new(None),
            git_ssh: AtomicBool::new(false),
            ids: Mutex::new(None),
            mise_on: AtomicBool::new(false),
            file_vault: AtomicBool::new(false),
            without_headless_flag: AtomicBool::new(false),
            free_space: Mutex::new(None),
        };
        // A host that restarts reuses the port its front stored. A port the first run took from
        // port 0 is free again the moment it quits, and a parallel test's port 0 or source port can
        // take it before the restart binds (a flaky `front-port-moved` problem), so every rig starts
        // on a quiet port, below the ephemeral ranges.
        store_front_port(&rig, puddle_test_kit::quiet_port());
        rig
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
        if !self.mise_on.load(Ordering::SeqCst) {
            let dir = self.dir.path().join("data").join("settings");
            let file = dir.join("global.json");
            if !file.exists() {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(&file, br#"{"schema_version": 2, "tools": {"mise": false}}"#)
                    .unwrap();
            }
        }
        let mut options = hermetic_options();
        options.launcher = if self.windowless.load(Ordering::SeqCst) {
            Arc::new(DesktopOnly(self.launcher.clone()))
        } else {
            self.launcher.clone()
        };
        options.secret_store = Some(self.secrets.clone());
        if self.file_vault.load(Ordering::SeqCst) {
            options.secret_store = None;
            options.secret_setup.headless = !self.without_headless_flag.load(Ordering::SeqCst);
            options.secret_setup.key_home = Some(self.dir.path().join("config"));
            options.secret_setup.tools = Some(puddle_secrets::ToolPaths::default());
            options.secret_setup.session = Some(puddle_secrets::LoginSession::NotGraphical);
        }
        options.git_ssh = self.git_ssh.load(Ordering::SeqCst);
        options
            .rewrite_gate
            .clone_from(&self.rewrite_gate.lock().unwrap());
        options.dev_cert = Some(self.dev_cert.clone() as Arc<dyn DevCertSource>);
        if let Some(ids) = self.ids.lock().unwrap().clone() {
            options.workspace_ids = Some(ids);
        }
        if let Some(space) = self.free_space.lock().unwrap().clone() {
            options.free_space = Some(space);
        }
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
    create_as(api, events, name, name).await;
}

/// Creates a workspace called `name` and checks that it got the id `id` (the hermetic options
/// make ids from names: `acme`, then `acme-2` while `acme` is taken).
async fn create_as(api: &Api, events: &mut support::Events, name: &str, id: &str) {
    let reply = api.post("/api/workspaces", &new_workspace(name)).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["id"], id, "{}", reply.body);
    events.until(ended(id), Duration::from_secs(20)).await;
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

// ---- where a test keeps its data ------------------------------------------------------------

// ---- the start-up order -------------------------------------------------------------------

// ---- the daemon end to end ----------------------------------------------------------------

/// The id of the list entry in `dir` on `workspace`'s list. Entry ids are generated, so a test
/// reads them from the workspace.
async fn entry_of(api: &Api, workspace: &str, dir: &str) -> String {
    let ws = api
        .get(&format!("/api/workspaces/{workspace}"))
        .await
        .json();
    let found = ws["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["directory"] == dir)
        .unwrap();
    found["entry"].as_str().unwrap().to_owned()
}

async fn checkouts_of(api: &Api, name: &str) -> serde_json::Value {
    let reply = api.get(&format!("/api/workspaces/{name}/checkouts")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
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
    let dir = support::scratch_dir();
    let password_file = dir.path().join("editor-password");
    std::fs::write(&password_file, "0".repeat(64)).unwrap();
    // Its port is taken, so the agent starts no server of its own and the stand-in answers.
    let mut editor = EditorServer::new("/nonexistent/data", "/nonexistent/work", 120);
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    editor.dir = "/nonexistent/release".into();
    editor.port = port;
    let sandbox = WorkspaceId::new(name).unwrap().sandbox_name();
    let route = rig.runtime.routes(&sandbox).first().unwrap().host.clone();
    let agent = puddle_agent::Agent::start(puddle_agent::Config {
        listen: std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), 0),
        target: Target::Unix(route),
        oom: None,
        ports: None,
        editor: Some(EditorConfig {
            server: editor,
            password_file: password_file.clone(),
            boot_id: dir.path().join("boot_id"),
            root: dir.path().join("root"),
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
    window_try_sends(origin, method, path).await.unwrap()
}

async fn window_try_sends(origin: &str, method: &str, path: &str) -> Option<String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let authority = origin.strip_prefix("http://").unwrap();
    let port: u16 = authority.rsplit(':').next().unwrap().parse().unwrap();
    let mut conn = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    conn.write_all(
        format!(
            "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .ok()?;
    let mut answer = Vec::new();
    conn.read_to_end(&mut answer).await.ok()?;
    Some(String::from_utf8_lossy(&answer).into_owned())
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

fn store_front_port(rig: &Rig, port: u16) {
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, port.to_string()).unwrap();
}

/// Puts a damaged global settings file where the host reads it, before the host starts.
fn damage_global_settings(rig: &Rig) -> PathBuf {
    let dir = rig.dir.path().join("data").join("settings");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("global.json");
    std::fs::write(&file, b"{ not json").unwrap();
    file
}

// ---- the shutdown order -------------------------------------------------------------------

// ---- after a restart ----------------------------------------------------------------------

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

/// The `workspace_op` records the activity log shows for `query`, oldest first, as the API
/// answers them.
async fn workspace_ops(query: &str, api: &Api) -> Vec<serde_json::Value> {
    let page = api
        .get(&format!("/api/audit?type=workspace_op&after=0&{query}"))
        .await;
    assert_eq!(page.status, 200, "{}", page.body);
    page.json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["record"].clone())
        .collect()
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

/// Creates `name` with direct SSH set `on` in its own settings. Settings belong to a workspace
/// that exists (its id is drawn at create), so it is made, stopped, set and started again.
async fn create_with_direct_ssh(
    api: &Api,
    events: &mut support::Events,
    name: &'static str,
    on: bool,
) {
    create(api, events, name).await;
    stop_and_wait(api, events, name).await;
    allow_direct_ssh(api, name, on).await;
    let reply = api.post(&format!("/api/workspaces/{name}/start"), "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended(name), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
}

/// The `Host` lines of Puddle's ssh config file (none when there is no file).
fn ssh_hosts(rig: &Rig) -> Vec<String> {
    std::fs::read_to_string(rig.puddle_ssh_config())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.strip_prefix("Host ").map(str::to_owned))
        .collect()
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

/// Seeds a workspace the list names whose volume is gone, then returns the rig and its host.
async fn host_with_listed_workspace_whose_volume_is_gone() -> (Rig, Host<FakeRuntime>) {
    let rig = Rig::named_ids();
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

// ---- credential injection: the CA, what is decrypted, the authors ---------------------------------

/// The path of the author file that holds `author`'s name (the file names follow the author's text).
fn author_file(files: &std::collections::BTreeMap<String, Vec<u8>>, author: &str) -> String {
    let wanted = format!("name = \"{author}\"");
    let hits: Vec<_> = files
        .iter()
        .filter(|(path, bytes)| {
            path.starts_with("/etc/puddle/git-author-")
                && String::from_utf8_lossy(bytes).contains(&wanted)
        })
        .map(|(path, _)| path.clone())
        .collect();
    assert_eq!(hits.len(), 1, "one author file for {author}: {hits:?}");
    hits[0].clone()
}

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

fn name(workspace: &str) -> WorkspaceId {
    WorkspaceId::new(workspace).unwrap()
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

/// Opens the host's database next to it and runs `sql` (a row the store would never write).
fn damage_database(rig: &Rig, sql: &str) {
    let db = rusqlite::Connection::open(rig.dir.path().join("data").join("puddle.db")).unwrap();
    db.execute_batch(sql).unwrap();
}

const UNREADABLE_GIT_ROW: &str = "INSERT INTO workspace_repos \
    (workspace_id, host, owner, repo, pull, push, created_at) \
    VALUES ('{}', 'github.com', 'acme', 'not a repo', 1, 1, 0)";

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

/// Attaches a new identity to `workspace`, as a user clicking would.
fn attach_a_new_identity(store: &puddle_store::Store, workspace: &WorkspaceId) {
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
        .create_identity(
            puddle_store::UserId::OWNER,
            puddle_store::IdentityDraft {
                label: "Ada".to_owned(),
                author: puddle_store::Author::new("Ada", "ada@example.org").unwrap(),
                credentials: vec![credential],
            },
        )
        .unwrap();
    // The first identity becomes the default, which a new workspace gets at its create; attach
    // is idempotent for this test's purpose.
    let _ = store.attach_identity(workspace, identity.id, None);
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

/// Waits until the host has read its stored proxy sign-ins (it does so off the start-up path).
async fn proxy_sign_ins_loaded(host: &Host<FakeRuntime>, count: usize) {
    eventually(
        &format!("{count} proxy sign-ins loaded"),
        Duration::from_secs(5),
        || host.proxy_passwords().proxies().len() == count,
    )
    .await;
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

/// Grows the root disk of `acme` to `size_mib` and waits for the operation to end; returns the
/// last progress event.
async fn grow_root_disk(
    api: &Api,
    events: &mut support::Events,
    size_mib: u64,
) -> serde_json::Value {
    let reply = api
        .post(
            "/api/workspaces/acme/grow-root-disk",
            &format!(r#"{{"size_mib": {size_mib}}}"#),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "growing_root_disk");
    events.until(ended("acme"), Duration::from_secs(20)).await
}

fn gib(n: u32) -> puddle_compute::DiskSize {
    puddle_compute::DiskSize::gib(n)
}

/// Waits for the problem `key` to be there (or gone) in the problem list, and says what it is.
async fn problem_is(api: &Api, key: &str, present: bool) -> Option<(String, String)> {
    let state = if present { "present" } else { "gone" };
    puddle_test_kit::eventually_async(
        &format!("the problem {key} {state}"),
        Duration::from_secs(10),
        || async {
            let found = problems_of(api)
                .await
                .into_iter()
                .find(|p| p.0 == key)
                .map(|p| (p.2, p.3));
            (found.is_some() == present).then_some(found)
        },
    )
    .await
}

// ---- the start folder and the editor's target --------------------------------------------

async fn put_start_folder(api: &Api, workspace: &str, path: Option<&str>) -> support::Reply {
    api.put(
        &format!("/api/workspaces/{workspace}/start-folder"),
        &json!({ "path": path }).to_string(),
    )
    .await
}

// ---- reports from the guest agent ---------------------------------------------------------

async fn wait_for_problem(host: &Host<FakeRuntime>, key: &str) -> puddle_types::Problem {
    eventually(&format!("the problem {key}"), APPLY, || {
        host.problems().list().into_iter().find(|p| p.key == key)
    })
    .await
}

// ---- ports ---------------------------------------------------------------------------------

// ---- starting from a checkout on this computer ------------------------------------------------

// ---- workspace disks the list does not name ------------------------------------------------

// ---- the terminal ------------------------------------------------------------------------

// ---- the managed tools' environment -----------------------------------------------------------

async fn restart(api: &Api, events: &mut support::Events, id: &'static str) {
    api.post(&format!("/api/workspaces/{id}/stop"), "").await;
    events.until(ended(id), Duration::from_secs(20)).await;
    api.post(&format!("/api/workspaces/{id}/start"), "").await;
    events.until(ended(id), Duration::from_secs(20)).await;
}

// ---- mise after a start -----------------------------------------------------------------------

fn mise_fetches(rig: &Rig) -> Vec<(Vec<String>, Option<String>)> {
    rig.guest.mise_fetches.lock().unwrap().clone()
}
