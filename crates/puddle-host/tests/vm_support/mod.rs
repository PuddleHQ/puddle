// SPDX-License-Identifier: GPL-3.0-or-later
//! What the VM tests of the host share (`vm_host.rs`, `vm_capture.rs`, `vm_git.rs`): a host on a
//! real msb runtime in the test's own scratch home, and a command run as root in a workspace's guest.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]
#![allow(dead_code, reason = "each test binary uses some of the helpers")]

use std::time::Duration;

use std::time::SystemTime;

use puddle_certs::{CorporateRoots, SOURCES, StoreSnapshot};
use puddle_compute::{ExecRequest, Runtime, Sandbox};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_e2e::lab::{NameProxy, WebHost, WebOptions};
use puddle_host::{
    GuestSettings, Host, HostConfig, HostError, HostOptions, HostPaths, Platform, RuntimeFactory,
    RuntimeInputs, START_STEPS, prepare,
};
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::MemoryStore;
use puddle_types::WorkspaceName;
use puddle_upstream::{Discovery, FakeOs, Mode, ProxyConfig, ProxyRules};
use puddle_vm_tests::{RuntimePair, Settings, VmEnv, test_owner};

/// The harness has set the environment and checked the runtime pair already.
#[derive(Default)]
pub(crate) struct HarnessPlatform {
    /// The corporate roots the host reports (the guest trusts them, the upstream TLS leg uses them).
    pub(crate) roots: CorporateRoots,
}

impl Platform for HarnessPlatform {
    fn pin_environment(&self, _: &RuntimeLayout) -> Result<(), HostError> {
        Ok(())
    }

    fn check_runtime(&self, _: &RuntimeLayout, _: &RuntimeVersion) -> Result<(), HostError> {
        Ok(())
    }

    fn corporate_roots(&self) -> Result<CorporateRoots, HostError> {
        Ok(self.roots.clone())
    }
}

/// Opens msb over the harness's private home and runtime pair.
pub(crate) struct HarnessFactory {
    settings: Settings,
    msb: std::path::PathBuf,
    libkrunfw: std::path::PathBuf,
    opened: std::sync::Mutex<Option<MsbRuntime>>,
}

impl RuntimeFactory for HarnessFactory {
    type Runtime = MsbRuntime;

    async fn open(&self, inputs: RuntimeInputs<'_>) -> Result<MsbRuntime, HostError> {
        let config = MsbConfig::new(
            self.settings.home(),
            &self.msb,
            &self.libkrunfw,
            inputs.guest_share,
        )
        .with_registry_proxy(inputs.pull_proxy.expose())
        .with_registry_roots(inputs.registry_roots)
        .with_runtime_log_level(self.settings.msb_log_level.clone())
        .with_keep_logs_dir(self.settings.kept_logs());
        let runtime = MsbRuntime::open(config).await?;
        *self.opened.lock().unwrap() = Some(runtime.clone());
        Ok(runtime)
    }
}

/// `options` with the host's upstream route set to the company proxy `proxy` (the only thing that
/// knows the fake hosts' names).
pub(crate) fn through_company_proxy(mut options: HostOptions, proxy: &NameProxy) -> HostOptions {
    let os = FakeOs::new(ProxyConfig {
        rules: ProxyRules::parse(&format!("http://127.0.0.1:{}", proxy.port())),
        ..ProxyConfig::default()
    });
    options.discovery = Some(Discovery::new(os, puddle_upstream::Config::default()));
    options
}

/// A platform that reports `roots` as the corporate roots: the guest trusts them and the host's
/// upstream TLS leg verifies against them, so a fake host signed by one is a host that works.
pub(crate) fn platform_trusting<'a>(
    roots: impl IntoIterator<Item = &'a rustls::pki_types::CertificateDer<'static>>,
) -> HarnessPlatform {
    let mut snapshot = StoreSnapshot::new();
    let mut distinct = std::collections::BTreeSet::new();
    for root in roots {
        snapshot.add(SOURCES[0], root.as_ref().to_vec());
        distinct.insert(root.as_ref().to_vec());
    }
    let platform = HarnessPlatform {
        roots: CorporateRoots::select(&snapshot, SystemTime::now()),
    };
    assert_eq!(
        platform.roots.certificates().len(),
        distinct.len(),
        "the fake hosts' roots are reported as corporate roots"
    );
    platform
}

/// A fake public website behind a company proxy: what a test uses when it needs "some allowed
/// host" to reach, so that it never depends on a real site being up. `web` answers every name
/// given to [`web_internet`] on ports 80 and 443; any other name goes to the real internet as it
/// stands. Start the host with `options` and `platform`.
pub(crate) struct WebInternet {
    pub(crate) web: WebHost,
    pub(crate) proxy: NameProxy,
    pub(crate) options: HostOptions,
    pub(crate) platform: HarnessPlatform,
}

/// Starts the fake website for `names` and the company proxy that holds them.
pub(crate) async fn web_internet(
    names: &[&str],
    web_options: WebOptions,
    options: HostOptions,
) -> WebInternet {
    let web = WebHost::start(names, web_options).await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, names);
    let options = through_company_proxy(options, &proxy);
    let platform = platform_trusting([web.root()]);
    WebInternet {
        web,
        proxy,
        options,
        platform,
    }
}

/// A host on a real msb runtime in the test's own scratch home.
pub(crate) struct VmHost {
    pub(crate) host: Host<MsbRuntime>,
    pub(crate) runtime: MsbRuntime,
    pub(crate) settings: Settings,
    /// The host's data folder.
    pub(crate) root: std::path::PathBuf,
    /// Where the host keeps the values of secrets: never the machine's own credential store.
    pub(crate) secrets: std::sync::Arc<MemoryStore>,
}

/// The host configuration over `root`: what [`start_vm_host_with`] and the second process of the
/// refusal test both build, so they name the same data folder the same way.
pub(crate) fn harness_config(
    settings: &Settings,
    pair: &RuntimePair,
    root: &std::path::Path,
    agent: std::path::PathBuf,
) -> HostConfig {
    let layout =
        RuntimeLayout::new(pair.msb.parent().unwrap().to_path_buf(), settings.home()).unwrap();
    let mut config = HostConfig::new(HostPaths::new(root), layout, GuestSettings::new(agent));
    config.upstream.discovery.mode = Mode::Direct;
    config.runtime_log_level.clone_from(&settings.msb_log_level);
    config
}

/// Starts a host over its own store and workspace folder, named by `tag` (a few characters), so
/// what one test creates (an identity, a rule, a workspace) is never in another's host. Tests of
/// one binary share the run's prefix, and a host over a shared folder reads the identities an
/// earlier test left.
pub(crate) async fn start_vm_host(tag: &str) -> VmHost {
    start_vm_host_with(tag, crate::support::hermetic_options()).await
}

pub(crate) async fn start_vm_host_with(tag: &str, options: HostOptions) -> VmHost {
    start_vm_host_config(tag, options, |_| {}).await
}

/// [`start_vm_host_with`] with `tweak` applied to the host's configuration first.
pub(crate) async fn start_vm_host_config(
    tag: &str,
    options: HostOptions,
    tweak: impl FnOnce(&mut HostConfig),
) -> VmHost {
    start_vm_host_platform(tag, options, &HarnessPlatform::default(), tweak).await
}

/// [`start_vm_host_config`] over `platform` (a host that reports corporate roots, say).
pub(crate) async fn start_vm_host_platform(
    tag: &str,
    options: HostOptions,
    platform: &HarnessPlatform,
    tweak: impl FnOnce(&mut HostConfig),
) -> VmHost {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    let settings = Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM test settings");
    let pair = settings.prepare().expect("msb runtime pair");
    let root = settings
        .claim_scratch_home(tag, &test_owner())
        .expect("one scratch home tag per test");
    std::fs::create_dir_all(&root).unwrap();
    let agent = std::path::PathBuf::from(
        std::env::var_os("PUDDLE_AGENT_BIN")
            .expect("PUDDLE_AGENT_BIN: the static puddle-agent (ci/build-agent.sh)"),
    );
    let mut config = harness_config(&settings, &pair, &root, agent);
    tweak(&mut config);
    let factory = HarnessFactory {
        settings: settings.clone(),
        msb: pair.msb.clone(),
        libkrunfw: pair.libkrunfw.clone(),
        opened: std::sync::Mutex::new(None),
    };
    let prepared = prepare(config, platform).unwrap();
    let secrets = std::sync::Arc::new(MemoryStore::new());
    let mut options = options;
    options.secret_store = Some(secrets.clone());
    let host = Host::start(prepared, &factory, options)
        .await
        .expect("the host starts");
    assert_eq!(host.steps(), START_STEPS);
    let runtime = factory.opened.lock().unwrap().clone().unwrap();
    VmHost {
        host,
        runtime,
        settings,
        root,
        secrets,
    }
}

/// Runs `script` as root in the guest and returns what it printed; fails the test if it fails.
pub(crate) async fn guest(runtime: &MsbRuntime, sandbox: &WorkspaceName, script: &str) -> String {
    guest_within(runtime, sandbox, script, Duration::from_secs(120)).await
}

/// [`guest`] with its own time limit, for a script that may take longer than two minutes.
pub(crate) async fn guest_within(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceName,
    script: &str,
    timeout: Duration,
) -> String {
    let handle = runtime.get(&sandbox.sandbox_name()).await.unwrap();
    let out = handle
        .exec(
            ExecRequest::sh(script)
                .as_user("root")
                .with_timeout(timeout),
        )
        .await
        .unwrap();
    assert!(
        out.status.success(),
        "{script}\n{}{}",
        out.stdout_text(),
        out.stderr_text()
    );
    format!("{}{}", out.stdout_text(), out.stderr_text())
}

/// Kills the VM of `sandbox` the hard way (no shutdown, no flush), the way a crash or a power cut
/// of the VMM would, and waits until the runtime sees it down.
pub(crate) async fn kill_vm(runtime: &MsbRuntime, settings: &Settings, sandbox: &WorkspaceName) {
    let harness = VmEnv::new(settings.clone()).await.unwrap();
    let handle = harness
        .scope(Box::pin(microsandbox::Sandbox::get(sandbox.as_str())))
        .await
        .unwrap();
    let pid = handle
        .local()
        .and_then(|l| l.pid)
        .expect("msb records the VM pid");
    let status = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
    } else {
        std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
    };
    assert!(status.unwrap().success(), "killing VM process {pid}");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let status = runtime
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.name == sandbox.as_str())
            .map(|s| s.status);
        if status.is_some_and(puddle_types::WorkspaceStatus::is_down) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "still {status:?} 60 s after the kill"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
