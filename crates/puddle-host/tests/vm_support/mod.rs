// SPDX-License-Identifier: GPL-3.0-or-later
//! What the VM tests of the host share (`vm_host.rs`, `vm_capture.rs`, `vm_git.rs`): a host on a
//! real msb runtime in the test's own scratch home, and a command run as root in a workspace's guest.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]
#![allow(dead_code, reason = "each test binary uses some of the helpers")]
#![allow(
    clippy::indexing_slicing,
    reason = "VM test: JSON read from the API, a missing key fails the test"
)]

pub(crate) mod kit;

use std::sync::Arc;
use std::time::Duration;

use std::time::SystemTime;

use puddle_certs::{CorporateRoots, SOURCES, StoreSnapshot};
use puddle_compute::{ExecRequest, Runtime, Sandbox};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_e2e::lab::{GitHost, NameProxy, Versions, WebHost, WebOptions};
use puddle_host::{
    GuestSettings, Host, HostConfig, HostError, HostOptions, HostPaths, Platform, RuntimeFactory,
    RuntimeInputs, START_STEPS, prepare,
};
use puddle_proxy::{BoxFuture, Resolver, SystemResolver};
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::MemoryStore;
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_test_kit::eventually;
use puddle_types::{DomainName, WorkspaceId};
use puddle_upstream::{Discovery, FakeOs, Mode, ProxyConfig, ProxyRules};
use puddle_vm_tests::{RuntimePair, Settings, VmEnv, test_owner};
use serde_json::json;

/// The harness has set the environment and checked the runtime pair already.
#[derive(Default)]
pub(crate) struct HarnessPlatform {
    /// The corporate roots the host reports (the guest trusts them, the upstream TLS leg uses them).
    pub(crate) roots: CorporateRoots,
}

impl Platform for HarnessPlatform {
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
        .with_protected_dir(inputs.data)
        .with_registry_proxy(inputs.pull_proxy.expose())
        .with_registry_roots(inputs.registry_roots)
        .with_runtime_log_level(self.settings.msb_log_level.clone())
        .with_keep_logs_dir(self.settings.kept_logs());
        let config = match inputs.ssh_login_key {
            Some(key) => config.with_ssh_key(key),
            None => config,
        };
        let runtime = MsbRuntime::open(config).await?;
        *self.opened.lock().unwrap() = Some(runtime.clone());
        Ok(runtime)
    }
}

/// The host's resolver for a fake site that has a real name (`github.com`): the name does not
/// exist here (only the fake company proxy knows it), and every other name goes to the system
/// resolver. Without this the proxy asks the machine's real DNS about the fake site on every
/// connect, and a runner whose resolver stalls for a few seconds stalls every connect with it.
/// A fake site under `.invalid` needs none of this: the proxy never looks that name up.
struct FakeNames {
    names: Vec<String>,
}

impl Resolver for FakeNames {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<std::net::SocketAddr>>> {
        if self
            .names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(name.as_str()))
        {
            return Box::pin(async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "a fake site: only the company proxy knows it",
                ))
            });
        }
        SystemResolver.resolve(name, port)
    }
}

/// `options` with the names in `names` that have a real name (`github.com`) as fake sites (see
/// [`FakeNames`]): the host never asks the real DNS about them. A name under `.invalid` is left
/// out, because the proxy never looks it up. [`web_internet`] does this for its names.
pub(crate) fn with_fake_names<'a>(
    mut options: HostOptions,
    names: impl IntoIterator<Item = &'a str>,
) -> HostOptions {
    let real_names: Vec<String> = names
        .into_iter()
        .filter(|n| !n.to_ascii_lowercase().ends_with(".invalid"))
        .map(str::to_owned)
        .collect();
    if !real_names.is_empty() {
        options.resolver = Some(Arc::new(FakeNames { names: real_names }));
    }
    options
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
    /// The fake `github.com` of [`web_internet_with_github`]; `None` for [`web_internet`].
    pub(crate) git: Option<GitHost>,
}

/// The repositories the tests create workspaces from, as `github.com` has them: public, on branch
/// `main`, one commit each.
pub(crate) const GITHUB_REPOS: [&str; 2] = ["octocat/Hello-World", "octocat/Spoon-Knife"];

/// The fake `github.com`: a git host answering to that name with [`GITHUB_REPOS`].
async fn github() -> GitHost {
    let git = GitHost::start("github.com", Versions::Both).await;
    for repo in GITHUB_REPOS {
        git.add_repo(repo, false);
    }
    git
}

/// Starts the fake website for `names` and the company proxy that holds them. A name that is not
/// a real service's takes the `.invalid` zone, which the proxy answers "no such name" without a
/// lookup (RFC 6761); a real service's name (`github.com`) is made unresolvable by [`FakeNames`].
pub(crate) async fn web_internet(
    names: &[&str],
    web_options: WebOptions,
    options: HostOptions,
) -> WebInternet {
    build_web_internet(names, web_options, options, None).await
}

/// [`web_internet`] with a fake `github.com` (see [`GITHUB_REPOS`]) behind the same proxy, for a
/// test whose workspace clones. The host's report of corporate roots names both roots.
pub(crate) async fn web_internet_with_github(
    names: &[&str],
    web_options: WebOptions,
    options: HostOptions,
) -> WebInternet {
    build_web_internet(names, web_options, options, Some(github().await)).await
}

async fn build_web_internet(
    names: &[&str],
    web_options: WebOptions,
    options: HostOptions,
    git: Option<GitHost>,
) -> WebInternet {
    let web = WebHost::start(names, web_options).await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, names);
    let mut fake_names: Vec<&str> = names.to_vec();
    let mut roots = vec![web.root()];
    if let Some(git) = &git {
        proxy.route("github.com:443", git.addr());
        fake_names.push("github.com");
        roots.push(git.root());
    }
    let options = with_fake_names(through_company_proxy(options, &proxy), fake_names);
    let platform = platform_trusting(roots);
    WebInternet {
        web,
        proxy,
        options,
        platform,
        git,
    }
}

/// A fake `github.com` alone, behind a company proxy: for a test that only clones.
pub(crate) struct GithubInternet {
    pub(crate) git: GitHost,
    pub(crate) proxy: NameProxy,
    pub(crate) options: HostOptions,
    pub(crate) platform: HarnessPlatform,
}

/// Starts the fake `github.com` (see [`GITHUB_REPOS`]) and the company proxy that holds it.
pub(crate) async fn github_internet(options: HostOptions) -> GithubInternet {
    let git = github().await;
    let proxy = NameProxy::start(&[("github.com:443", git.addr())]).await;
    let options = with_fake_names(through_company_proxy(options, &proxy), ["github.com"]);
    let platform = platform_trusting([git.root()]);
    GithubInternet {
        git,
        proxy,
        options,
        platform,
    }
}

/// [`start_vm_host_config`] on a fake `github.com`: the host clones from it, never from the real
/// one. Keep the returned internet alive while the host runs.
pub(crate) async fn start_vm_host_github(tag: &str) -> (VmHost, GithubInternet) {
    let net = github_internet(crate::support::named_id_options()).await;
    let vm = start_vm_host_platform(tag, net.options.clone(), &net.platform, |_| {}).await;
    (vm, net)
}

/// A host on a real msb runtime in the test's own scratch home.
pub(crate) struct VmHost {
    pub(crate) host: Host<MsbRuntime>,
    pub(crate) runtime: MsbRuntime,
    pub(crate) settings: Settings,
    /// The host's data folder.
    pub(crate) root: std::path::PathBuf,
    /// Where the host keeps the values of secrets: never the machine's own credential store.
    pub(crate) secrets: Arc<MemoryStore>,
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

/// The folder the guest tools of the VM tests are kept in between runs: `PUDDLE_VM_TOOLS_CACHE` (CI
/// restores it) or `~/.cache/puddle-vm-tools`; `None` when neither can be named (every run is cold).
pub(crate) fn cache_root() -> Option<std::path::PathBuf> {
    std::env::var_os("PUDDLE_VM_TOOLS_CACHE")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::Path::new(&h).join(".cache/puddle-vm-tools"))
        })
}

/// Points the run's msb image cache at `PUDDLE_VM_IMAGE_CACHE` when that is set, so the images the
/// tests pull (`docker:dind`, the devcontainers base) come from disk after the first run and no
/// registry is asked. msb looks an image up in its cache first. Left alone when the run's cache
/// already exists (an earlier test of the run made it) or the variable is unset (a cold run).
fn share_image_cache(settings: &Settings) {
    #[cfg(unix)]
    if let Some(shared) = std::env::var_os("PUDDLE_VM_IMAGE_CACHE").map(std::path::PathBuf::from) {
        let link = settings.home().join("cache");
        if std::fs::symlink_metadata(&link).is_err() {
            std::fs::create_dir_all(&shared).unwrap();
            // Another test of the run may have made the link meanwhile; either way it points here.
            let _ = std::os::unix::fs::symlink(&shared, &link);
        }
    }
    #[cfg(not(unix))]
    let _ = settings;
}

/// Starts a host over its own store and workspace folder, named by `tag` (a few characters), so
/// what one test creates (an identity, a rule, a workspace) is never in another's host. Tests of
/// one binary share the run's prefix, and a host over a shared folder reads the identities an
/// earlier test left.
pub(crate) async fn start_vm_host(tag: &str) -> VmHost {
    start_vm_host_with(tag, crate::support::named_id_options()).await
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
    share_image_cache(&settings);
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
    let secrets = Arc::new(MemoryStore::new());
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
pub(crate) async fn guest(runtime: &MsbRuntime, sandbox: &WorkspaceId, script: &str) -> String {
    guest_within(runtime, sandbox, script, Duration::from_secs(120)).await
}

/// [`guest`] with its own time limit, for a script that may take longer than two minutes.
pub(crate) async fn guest_within(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceId,
    script: &str,
    timeout: Duration,
) -> String {
    guest_with_third_party(runtime, sandbox, script, timeout, None).await
}

/// [`guest`] for a script that reaches a live third party on purpose (`hosts`, for example a
/// package mirror): a timeout or a failure then says that host is the thing to check.
pub(crate) async fn guest_live(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceId,
    script: &str,
    hosts: &str,
) -> String {
    guest_with_third_party(
        runtime,
        sandbox,
        script,
        Duration::from_secs(120),
        Some(hosts),
    )
    .await
}

async fn guest_with_third_party(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceId,
    script: &str,
    timeout: Duration,
    hosts: Option<&str>,
) -> String {
    let note = hosts.map_or_else(String::new, |h| {
        format!(" (this test reaches the live third party {h}; an outage there fails it)")
    });
    let handle = runtime.get(&sandbox.sandbox_name()).await.unwrap();
    let out = handle
        .exec(
            ExecRequest::sh(script)
                .as_user("root")
                .with_timeout(timeout),
        )
        .await
        .map_err(|e| format!("the guest script failed to finish: {e}{note}\n{script}"))
        .unwrap();
    assert!(
        out.status.success(),
        "{script}\n{}{}{note}",
        out.stdout_text(),
        out.stderr_text()
    );
    format!("{}{}", out.stdout_text(), out.stderr_text())
}

/// Runs `script` in a login shell as `user`, the way a terminal of the workspace does and
/// returns its exit code and what it printed. The script is an argument, not stdin, so a tool that
/// reads stdin cannot eat the rest of it.
pub(crate) async fn guest_as(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceId,
    user: &str,
    script: &str,
    timeout: Duration,
) -> (i32, String) {
    let handle = runtime.get(&sandbox.sandbox_name()).await.unwrap();
    let out = handle
        .exec(
            ExecRequest::sh(format!(
                "cd && exec sh -l -c '{}'",
                script.replace('\'', "'\\''")
            ))
            .as_user(user)
            .with_timeout(timeout),
        )
        .await
        .unwrap_or_else(|e| panic!("the guest script did not finish: {e}\n{script}"));
    (
        out.status.code,
        format!("{}{}", out.stdout_text(), out.stderr_text()),
    )
}

/// Kills the VM of `sandbox` the hard way (no shutdown, no flush), the way a crash or a power cut
/// of the VMM would, and waits until the runtime sees it down.
pub(crate) async fn kill_vm(runtime: &MsbRuntime, settings: &Settings, sandbox: &WorkspaceId) {
    let harness = VmEnv::new(settings.clone()).await.unwrap();
    let pid = harness
        .process_id_of(sandbox.as_str())
        .await
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
    puddle_test_kit::eventually_async(
        "the VM down after the kill",
        Duration::from_secs(60),
        || async {
            let status = runtime
                .list()
                .await
                .unwrap()
                .into_iter()
                .find(|s| s.name == sandbox.as_str())
                .map(|s| s.status);
            if status.is_some_and(puddle_types::WorkspaceStatus::is_down) {
                Ok(())
            } else {
                Err(format!("still {status:?}"))
            }
        },
    )
    .await;
}

/// What `puddle ssh-bridge <endpoint>` does: relay standard input and output to the endpoint. A test binary that
/// is its own `ProxyCommand` (the ssh config entry names it as the bridge program) calls this for `ssh-bridge <endpoint>`.
#[expect(
    clippy::print_stderr,
    reason = "the bridge's own message to ssh, which shows it"
)]
pub(crate) fn ssh_bridge(endpoint: Option<&str>) -> std::process::ExitCode {
    let Some(endpoint) = endpoint else {
        eprintln!("puddle: missing endpoint");
        return std::process::ExitCode::from(2);
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(puddle_ssh::bridge::run(
        std::path::Path::new(endpoint),
        tokio::io::stdin(),
        tokio::io::stdout(),
    ));
    rt.shutdown_background();
    match result {
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("puddle: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// [`create_workspace`] with `extra` fields (an image, a memory size) in the request.
pub(crate) async fn create_workspace_from(
    vm: &VmHost,
    api: &crate::support::Api,
    tag: &str,
    allowed: &[&str],
    extra: serde_json::Value,
) -> WorkspaceId {
    let name = format!("{}-{tag}", vm.settings.prefix.as_str());
    let workspace = WorkspaceId::new(&name).unwrap();
    for site in allowed {
        vm.host
            .store()
            .add_rule(&NewRule {
                scope: Scope::Workspace(workspace.clone()),
                pattern: Pattern::parse(site).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }
    let mut events = api.events().await;
    let reply = api
        .post("/api/workspaces", &{
            let mut body = extra;
            body["name"] = json!(name);
            body.to_string()
        })
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let name: &'static str = Box::leak(name.into_boxed_str());
    let end = events
        .until(crate::support::ended(name), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    workspace
}

/// Shell prelude of every command the Docker test runs as the session user: a millisecond clock
/// and a Docker config of its own, so no proxy variable from the CLI's config reaches a container.
/// `dsudo` is `sudo docker` where the image has `sudo`, and plain `docker` (the `docker` group,
/// which the boot grants the session user) where it has none, as `docker:dind` has not.
pub(crate) const DOCKER_PRELUDE: &str = r#"now() { echo $(( $(date +%s%N) / 1000000 )); }
export DOCKER_CONFIG=/tmp/docker-without-proxy
mkdir -p "$DOCKER_CONFIG"
dsudo() { if [ -x /usr/bin/sudo ] || [ -x /usr/local/bin/sudo ]; then sudo env DOCKER_CONFIG="$DOCKER_CONFIG" docker "$@"; else docker "$@"; fi; }
"#;

/// Builds the probe image `FROM scratch`: busybox and the loader it needs, copied from the guest's
/// own files, and nothing else. No registry is involved.
pub(crate) const PROBE_IMAGE: &str = r"d=/tmp/probe-image
mkdir -p $d/rootfs/bin $d/rootfs/lib
cp /bin/busybox $d/rootfs/bin/busybox
for l in /lib/ld-musl-*.so.1; do cp -L $l $d/rootfs/lib/; done
printf 'FROM scratch\nCOPY rootfs/ /\n' > $d/Dockerfile
dsudo build -q -t probe:local $d";

/// Runs `script` as `agent` (the session user) with [`DOCKER_PRELUDE`] first.
pub(crate) async fn as_agent(vm: &VmHost, workspace: &WorkspaceId, script: &str) -> (i32, String) {
    guest_as(
        &vm.runtime,
        workspace,
        "agent",
        &format!("{DOCKER_PRELUDE}{script}"),
        Duration::from_secs(180),
    )
    .await
}

/// The value after `key=` on the first line of `text` that has it.
pub(crate) fn field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .find_map(|l| l.split_whitespace().find_map(|w| w.strip_prefix(key)))
        .unwrap_or_else(|| panic!("no {key} in: {text}"))
}

/// How many requests the fake website saw with `Host: host`.
pub(crate) fn reached_by_host(web: &WebHost, host: &str) -> usize {
    web.seen()
        .iter()
        .filter(|r| r.headers_named("host") == [host])
        .count()
}

/// The number of allowed connection records to `host:port` in the audit.
pub(crate) fn allowed_connections(vm: &VmHost, host: &str, port: u16) -> usize {
    vm.host
        .store()
        .audit_lines(0, 100_000)
        .unwrap()
        .iter()
        .filter(|(_, line)| {
            let record: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
            record["type"] == "connection"
                && record["host"] == host
                && record["port"] == port
                && record["decision"] == "allow"
        })
        .count()
}

/// Waits until the audit holds more allowed connection records to `host:port` than `before`.
///
/// The proxy writes a connection's record when the connection ends, which is after the guest
/// tool that opened it has exited and the test has read its output, so reading the audit
/// right away races the proxy's write.
pub(crate) async fn wait_for_connection_record(vm: &VmHost, host: &str, port: u16, before: usize) {
    eventually(
        &format!("a connection record for {host}:{port}"),
        Duration::from_secs(20),
        || {
            if allowed_connections(vm, host, port) > before {
                return Ok(());
            }
            let lines: Vec<String> = vm
                .host
                .store()
                .audit_lines(0, 100_000)
                .unwrap()
                .into_iter()
                .map(|(_, line)| line)
                .filter(|line| line.contains(host))
                .collect();
            Err(format!("audit lines naming it: {lines:#?}"))
        },
    )
    .await;
}

/// A container's fetch of `url` on `network`: what proxy variables it sees, then wget's output, the
/// exit code and the milliseconds it took.
pub(crate) fn container_fetch(docker: &str, network: &str, url: &str) -> String {
    format!(
        "s=$(now); {docker} run --rm --pull=never --network {network} probe:local /bin/busybox \
         sh -c '/bin/busybox env | /bin/busybox grep -i proxy; /bin/busybox wget -T 10 -q -O - {url}' 2>&1; \
         echo \"exit=$? ms=$(( $(now) - s ))\""
    )
}
