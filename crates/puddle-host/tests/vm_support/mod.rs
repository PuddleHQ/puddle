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

use puddle_certs::CorporateRoots;
use puddle_compute::{ExecRequest, Runtime, Sandbox};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_host::{
    GuestSettings, Host, HostConfig, HostError, HostOptions, HostPaths, Platform, RuntimeFactory,
    RuntimeInputs, START_STEPS, prepare,
};
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::MemoryStore;
use puddle_types::WorkspaceName;
use puddle_upstream::Mode;
use puddle_vm_tests::{RuntimePair, Settings, test_owner};

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

/// [`guest`] with its own time limit (a search of the whole disk takes longer than a command).
pub(crate) async fn guest_within(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceName,
    script: &str,
    limit: Duration,
) -> String {
    let handle = runtime.get(&sandbox.sandbox_name()).await.unwrap();
    let out = handle
        .exec(ExecRequest::sh(script).as_user("root").with_timeout(limit))
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
