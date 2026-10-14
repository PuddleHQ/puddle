// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest cases at tier V that need the product's boot hook: the guest runs puddle's real
//! `puddle-agent` (started by `boot.sh`, from `PUDDLE_AGENT_BIN`, the static build of
//! `ci/build-agent.sh`) against a real proxy on the host, and puddle's own read-only mounts.
//!
//! HG-04 and HG-05: a process in the guest asks the agent for a CONNECT to each of puddle's own
//! listeners, in every loopback spelling, with every local toggle on and an allow rule for the
//! address; every one is refused as a puddle endpoint. HG-23: every mount puddle puts into the
//! guest refuses writes, whatever root tries, and no host data folder is visible in the guest.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "VM test helpers: a failed step fails the test"
)]

mod support;

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, Services, SettingsRepo,
};
use puddle_boot::{
    BOOT_SH, BootHook, BootPlan, Gate, GatedSandbox, with_boot_mounts, write_assets,
};
use puddle_compute::{ExecOutput, ExecRequest, Runtime, VsockRoute};
use puddle_compute_msb::MsbRuntime;
use puddle_ipc::IpcRoot;
use puddle_netpolicy::{EndpointKind, LocalAccess, NetPolicy, PuddleEndpoints};
use puddle_proxy::testing::StaticResolver;
use puddle_proxy::{Proxy, PullProxy};
use puddle_store::{Actor, Effect, Limits, ManualClock, NewRule, Pattern, Scope, Store};
use puddle_types::{ImageRef, LocalCategory, NullSink, WorkspaceName};
use puddle_vm_tests::{DEBIAN_DEVCONTAINER, Settings};
use support::{cleanup, spec};

/// The guest port puddle routes (the agent's default target is `vsock://2:5000`).
const ROUTE_PORT: u32 = 5000;

const NOW: u64 = 1_800_000_000_000;

/// Sends `CONNECT <authority>` to the agent's listener and prints the status line and the header
/// lines, then `BODY <body>` when the answer has one.
const CONNECT_PROBE: &str = r#"
authority=$1
exec 3<>/dev/tcp/127.0.0.1/3128 || exit 9
printf 'CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n' "$authority" "$authority" >&3
length=0
while IFS= read -r -t 10 line <&3; do
  line=${line%$'\r'}
  [ -z "$line" ] && break
  printf '%s\n' "$line"
  case $line in *-[Ll]ength:*) length=${line#*: } ;; esac
done
if [ "$length" -gt 0 ]; then
  IFS= read -r -t 5 -N "$length" body <&3
  printf 'BODY %s\n' "$body"
fi
"#;

/// A sandbox booted by the real hook with puddle's mounts, the real agent in it and a route.
struct Booted {
    rt: MsbRuntime,
    sb: GatedSandbox<<MsbRuntime as Runtime>::Sandbox>,
    name: puddle_types::SandboxName,
    /// The host directory the boot files were written to.
    assets: PathBuf,
    agent_bytes: Vec<u8>,
}

impl Booted {
    async fn sh(&self, script: &str) -> ExecOutput {
        self.sb
            .exec(ExecRequest::sh(script).as_user("root"))
            .await
            .expect("exec")
    }

    async fn finish(self) {
        self.sb.stop().await.unwrap();
        cleanup(&self.rt, &[&self.name]).await;
    }
}

/// Boots a devcontainer sandbox through [`BootHook`] with the boot scripts and the agent mounted
/// read-only, and the vsock route to `route` if given.
async fn boot(settings: &Settings, tag: &str, route: Option<&std::path::Path>) -> Booted {
    let rt = support::runtime(settings).await;
    let image = ImageRef::new(DEBIAN_DEVCONTAINER).unwrap();
    let config = rt.pull_image(&image).await.expect("pull");
    let plan = BootPlan::builder(&config).build().unwrap();
    let assets = rt.config().guest_share.join(format!("hostile-{tag}"));
    let mounts = write_assets(&assets).unwrap();
    let built = PathBuf::from(
        std::env::var_os("PUDDLE_AGENT_BIN")
            .expect("PUDDLE_AGENT_BIN: the static puddle-agent (ci/build-agent.sh)"),
    );
    let agent = assets.join("puddle-agent");
    std::fs::copy(&built, &agent).expect("copy the agent into the guest-share root");
    let agent_bytes = std::fs::read(&agent).unwrap();
    let mut s = spec(settings, tag);
    if let Some(route) = route {
        s = s.with_route(VsockRoute::new(ROUTE_PORT, route));
    }
    let name = s.name.clone();
    let s = with_boot_mounts(s, mounts, Some(&agent));
    let sb = BootHook::new()
        .create(&rt, s, &plan, &Gate::new())
        .await
        .expect("boot");
    Booted {
        rt,
        sb,
        name,
        assets,
        agent_bytes,
    }
}

/// A CONNECT through the guest's agent: the status line, the lower-cased header lines and the body.
struct Answer {
    status: String,
    headers: Vec<String>,
    body: String,
}

async fn connect_through_the_agent(guest: &Booted, authority: &str) -> Answer {
    let out = guest
        .sb
        .exec(ExecRequest::new("bash", ["-c", CONNECT_PROBE, "probe", authority]).as_user("root"))
        .await
        .expect("exec");
    assert_eq!(
        out.status.code,
        0,
        "{authority}: no answer: {}",
        out.stderr_text()
    );
    let text = out.stdout_text().into_owned();
    let mut lines = text.lines();
    let status = lines.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    let mut body = String::new();
    for l in lines {
        match l.strip_prefix("BODY ") {
            Some(b) => b.clone_into(&mut body),
            None => headers.push(l.to_ascii_lowercase()),
        }
    }
    Answer {
        status,
        headers,
        body,
    }
}

/// A workspace proxy on the host with the most permissive address guard there is (every local
/// category on, wildcards reaching local) and a route the guest's agent dials.
struct HostRig {
    store: Arc<Store>,
    endpoints: PuddleEndpoints,
    _route: puddle_proxy::Route,
    _root: IpcRoot,
    route_path: PathBuf,
}

fn host_rig(endpoints: PuddleEndpoints) -> HostRig {
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open_in_memory(clock, Limits::default()).unwrap());
    let access = LocalCategory::ALL
        .into_iter()
        .fold(LocalAccess::NONE, |a, c| a.with_toggle(c, true))
        .with_wildcards_reach_local(true);
    let guard = NetPolicy::new(Arc::new(access)).with_endpoints(endpoints.clone());
    let proxy = Arc::new(
        Proxy::new(store.clone(), Arc::new(NullSink))
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(guard)),
    );
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(
        root.listen().unwrap(),
        WorkspaceName::new("hostile-agent").unwrap(),
    );
    let route_path = route.endpoint().path().to_path_buf();
    HostRig {
        store,
        endpoints,
        _route: route,
        _root: root,
        route_path,
    }
}

/// Self-approval through the real agent in a real guest: every listener puddle has bound (the API,
/// the image-pull proxy and a stand-in for the listeners that come later) is refused as a puddle
/// endpoint, in every loopback spelling, with every local toggle on and an allow rule for the
/// literal address. Nothing is left pending for the user to approve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg04_hg05_a_guest_cannot_reach_puddles_own_listeners_through_the_agent() {
    let settings = support::settings();
    let endpoints = PuddleEndpoints::new();
    let services = Services::new(
        Arc::new(
            Store::open_in_memory(Arc::new(ManualClock::new(NOW)), Limits::default()).unwrap(),
        ),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        Arc::new(EventHub::default()),
        Arc::new(ManualClock::new(NOW)),
    )
    .with_endpoints(endpoints.clone());
    let api = ApiServer::bind(
        ApiConfig::default(),
        ApiToken::generate().unwrap(),
        services,
    )
    .await
    .unwrap()
    .spawn();
    let pull = PullProxy::bind(&endpoints).unwrap().serve().unwrap();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let _other = endpoints.register(other.local_addr().unwrap(), EndpointKind::Other);

    let rig = host_rig(endpoints);
    rig.store
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse("127.0.0.1").unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    let guest = boot(&settings, "hg04", Some(&rig.route_path)).await;

    let listeners: Vec<(SocketAddr, EndpointKind)> = rig.endpoints.list();
    let kinds: Vec<EndpointKind> = listeners.iter().map(|(_, k)| *k).collect();
    assert!(
        kinds.contains(&EndpointKind::Api) && kinds.contains(&EndpointKind::PullProxy),
        "the rig must register the API and the pull proxy: {kinds:?}"
    );
    assert_eq!(api.local_addr().ip().to_string(), "127.0.0.1");
    assert_eq!(pull.local_addr().ip().to_string(), "127.0.0.1");
    let mut failures = Vec::new();
    for (addr, kind) in listeners {
        let port = addr.port();
        for authority in [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::ffff:127.0.0.1]:{port}"),
            format!("0.0.0.0:{port}"),
        ] {
            let answer = connect_through_the_agent(&guest, &authority).await;
            let refused = answer.status.contains(" 403")
                && answer
                    .headers
                    .contains(&"x-puddle-blocked: puddle_endpoint".to_owned())
                && !answer
                    .headers
                    .iter()
                    .any(|h| h.starts_with("x-puddle-pending"))
                && answer.body.contains("puddle's own endpoints");
            if !refused {
                failures.push(format!(
                    "{kind} at {authority}: {:?} {:?} {:?}",
                    answer.status, answer.headers, answer.body
                ));
            }
        }
    }
    let pending = rig.store.open_pending(None).unwrap().len();
    guest.finish().await;
    pull.shutdown().await;
    assert!(failures.is_empty(), "{failures:#?}");
    assert_eq!(pending, 0, "a refused endpoint must not become a request");
}

/// Root's attempts to change the mounted file `target`, then whether anything changed.
const WRITE_ATTACK: &str = r#"
f=$1
before=$(sha256sum "$f" | cut -d' ' -f1)$(stat -c ' %a %U' "$f")
echo "append: $(sh -c ': >> "$0"' "$f" 2>&1)"
echo "write: $(sh -c 'echo x > "$0"' "$f" 2>&1)"
echo "truncate: $(truncate -s 0 "$f" 2>&1)"
echo "delete: $(rm -f "$f" 2>&1)"
echo "rename: $(mv "$f" "$f.moved" 2>&1)"
echo "chmod: $(chmod 777 "$f" 2>&1)"
echo "remount: $(mount -o remount,rw "$f" 2>&1)"
echo "append after remount: $(sh -c 'echo x >> "$0"' "$f" 2>&1)"
echo "truncate after remount: $(truncate -s 0 "$f" 2>&1)"
after=$(sha256sum "$f" | cut -d' ' -f1)$(stat -c ' %a %U' "$f")
[ "$before" = "$after" ] && echo "unchanged" || echo "CHANGED: $before -> $after"
[ -e "$f.moved" ] && echo "MOVED"
exit 0
"#;

/// Runs [`WRITE_ATTACK`] on `target` and lists what was not refused.
async fn write_attack(guest: &Booted, target: &str) -> (String, Vec<String>) {
    let out = guest
        .sb
        .exec(ExecRequest::new("sh", ["-c", WRITE_ATTACK, "attack", target]).as_user("root"))
        .await
        .expect("exec");
    let report = out.stdout_text().into_owned();
    let line = |step: &str| {
        report
            .lines()
            .find(|l| l.starts_with(&format!("{step}: ")))
            .unwrap_or("")
            .to_owned()
    };
    let mut failures = Vec::new();
    for step in [
        "append",
        "write",
        "truncate",
        "append after remount",
        "truncate after remount",
    ] {
        let l = line(step);
        if !(l.contains("Read-only file system") || l.contains("Permission denied")) {
            failures.push(format!("{target}: {step} was not refused: {l:?}"));
        }
    }
    for step in ["delete", "rename", "chmod"] {
        if line(step) == format!("{step}: ") {
            failures.push(format!("{target}: {step} succeeded"));
        }
    }
    if !report.lines().any(|l| l == "unchanged") || report.contains("MOVED") {
        failures.push(format!("{target}: the file changed:\n{report}"));
    }
    (report, failures)
}

/// Every mount puddle puts into the guest refuses writes from root, and the host's data folder is
/// not there to see. The mounts are the boot hook's: `boot.sh`, `agent-supervise.sh` and the
/// agent binary. Root tries to append, truncate, delete, rename and chmod each one, and to
/// remount it read-write and write again; afterwards the guest's view and the host's files are
/// byte for byte what was mounted. A sentinel file in a data folder and one beside the mounted
/// files on the host must not be findable anywhere in the guest, and `/proc/mounts` must name
/// no host path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg23_every_boot_mount_refuses_writes_and_no_data_folder_is_visible() {
    let settings = support::settings();
    let data = settings.home().join("hostile-data-folder");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("CANARY-hg23-store.db"), b"CANARY").unwrap();
    let guest = boot(&settings, "hg23", None).await;
    std::fs::write(guest.assets.join("CANARY-hg23-beside"), b"CANARY").unwrap();

    let mounted = [
        "/puddle/boot.sh",
        "/puddle/agent-supervise.sh",
        "/puddle/puddle-agent",
    ];
    let table = guest
        .sh("cat /proc/mounts /proc/self/mountinfo")
        .await
        .stdout_text()
        .into_owned();
    eprintln!("{table}");
    let targets = guest
        .sh(r#"awk '$2 == "/puddle" || index($2, "/puddle/") == 1 { print $2 }' /proc/mounts | sort -u"#)
        .await
        .stdout_text()
        .into_owned();
    let mut targets: Vec<&str> = targets.lines().collect();
    for m in mounted {
        assert!(targets.contains(&m), "{m} is not a mount: {targets:?}");
    }
    targets.retain(|t| *t != "/puddle");

    let mut failures = Vec::new();
    for target in &targets {
        let (report, refused) = write_attack(&guest, target).await;
        eprintln!("{target}:\n{report}");
        failures.extend(refused);
    }

    // What the guest can see of the host: no host path in the mount tables, and neither sentinel
    // is findable anywhere (the search crosses every mount).
    let leaks: Vec<String> = [settings.home(), guest.rt.config().guest_share.clone()]
        .iter()
        .map(|p| p.display().to_string())
        .filter(|p| table.contains(p.as_str()))
        .collect();
    let found = guest
        .sh(r"find / \( -path /proc -o -path /sys \) -prune -o -name 'CANARY-hg23-*' -print 2>/dev/null")
        .await
        .stdout_text()
        .into_owned();

    // What the guest could have changed on the host.
    let mut changed = Vec::new();
    if std::fs::read_to_string(guest.assets.join("boot.sh")).unwrap() != BOOT_SH {
        changed.push("boot.sh");
    }
    if std::fs::read(guest.assets.join("puddle-agent")).unwrap() != guest.agent_bytes {
        changed.push("puddle-agent");
    }
    assert!(guest.assets.join("agent-supervise.sh").is_file());
    guest.finish().await;
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(
        leaks.is_empty(),
        "the mount tables name host paths: {leaks:?}"
    );
    assert!(found.is_empty(), "the guest can see host files: {found}");
    assert!(changed.is_empty(), "host files changed: {changed:?}");
}
