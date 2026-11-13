// SPDX-License-Identifier: GPL-3.0-or-later
//! JetBrains Gateway's own SSH client against a workspace's direct SSH entry, on a real microVM.
//!
//! Gateway runs headless with a small probe plugin (`tests/gateway_probe/`) that calls Gateway's
//! own code: its platform SSH client (sshj as Gateway bundles it, the ssh config read through
//! `ssh -G`, its `ProxyCommand` runner and host key check), then Gateway's own bootstrap (check
//! the host, upload and run its remote-dev-worker, the memory, CPU, file locking, disk and port
//! forwarding checks). The ssh config entry names this binary as the bridge program, as
//! `vm_desktop_attach` does.
//!
//! Needs `PUDDLE_GATEWAY_DIR`: the pinned Gateway that `ci/fetch-gateway.sh` unpacks. The probe is
//! built from its sources with Gateway's own `javac` on each run.
#![expect(
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "a plain test program for a VM: a failed step fails the run; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::process::ExitCode;

type Case = (&'static str, fn());

#[cfg(unix)]
const CASES: &[Case] = &[(
    "vm_gateway_attach_runs_gateways_own_ssh_client_and_bootstrap",
    gateway::runs_gateways_own_ssh_client_and_bootstrap,
)];

#[cfg(not(unix))]
const CASES: &[Case] = &[];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("ssh-bridge") {
        return vm_support::ssh_bridge(args.get(1).map(String::as_str));
    }
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            for (name, _) in CASES {
                println!("{name}: test");
            }
        }
        return ExitCode::SUCCESS;
    }
    let wanted: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    for (name, body) in CASES {
        if wanted.is_empty() || wanted.iter().any(|w| name.contains(w.as_str())) {
            body();
            println!("test {name} ... ok");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
#[expect(
    clippy::expect_used,
    reason = "a plain test program for a VM: a failed step fails the run"
)]
mod gateway {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use puddle_host::{HostOptions, VsCodeLauncher};
    use puddle_types::WorkspaceId;
    use serde_json::json;

    use crate::support::{Api, ended, named_id_options};
    use crate::vm_support::{VmHost, start_vm_host_config};

    /// A port in the guest for the probe's own listener, which it reaches through a forward.
    const GUEST_PORT: &str = "18765";

    /// How long the probe may run before the test kills Gateway and shows what it got.
    const PROBE_LIMIT: Duration = Duration::from_secs(360);

    fn env_dir(name: &str) -> PathBuf {
        let dir = PathBuf::from(
            std::env::var_os(name)
                .unwrap_or_else(|| panic!("{name} is not set (see the module docs)")),
        );
        assert!(dir.is_dir(), "{name}={} is not a folder", dir.display());
        dir
    }

    pub(super) fn runs_gateways_own_ssh_client_and_bootstrap() {
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(run());
            })
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    }

    /// Compiles the probe plugin against Gateway's own jars and packs it as a plugin folder under
    /// `dir`; returns the plugins folder.
    fn build_probe(gateway_dir: &Path, dir: &Path) -> PathBuf {
        let sources = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/gateway_probe");
        let mut jars = Vec::new();
        for lib in [
            "lib",
            "plugins/platform-ssh-plugin/lib",
            "plugins/platform-ssh-plugin/lib/modules",
        ] {
            for entry in std::fs::read_dir(gateway_dir.join(lib)).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|e| e == "jar") {
                    jars.push(path.display().to_string());
                }
            }
        }
        let classes = dir.join("probe-classes");
        std::fs::create_dir_all(classes.join("META-INF")).unwrap();
        let javac = std::process::Command::new(gateway_dir.join("jbr/bin/javac"))
            .args(["-nowarn", "-encoding", "UTF-8", "-d"])
            .arg(&classes)
            .arg("-cp")
            .arg(jars.join(":"))
            .arg(sources.join("ProbeStarter.java"))
            .arg(sources.join("GatewayDeploy.java"))
            .output()
            .expect("Gateway's javac runs");
        assert!(
            javac.status.success(),
            "javac: {}",
            String::from_utf8_lossy(&javac.stderr)
        );
        std::fs::copy(
            sources.join("plugin.xml"),
            classes.join("META-INF/plugin.xml"),
        )
        .unwrap();
        let plugins = dir.join("plugins");
        let lib = plugins.join("puddle-probe/lib");
        std::fs::create_dir_all(&lib).unwrap();
        let jar = std::process::Command::new(gateway_dir.join("jbr/bin/java"))
            .arg(sources.join("MakeJar.java"))
            .arg(&classes)
            .arg(lib.join("puddle-probe.jar"))
            .output()
            .expect("Gateway's java runs");
        assert!(
            jar.status.success(),
            "jar: {}",
            String::from_utf8_lossy(&jar.stderr)
        );
        plugins
    }

    /// Gateway's own launcher, headless, with its config, caches and logs under `dir`.
    fn gateway(
        gateway_dir: &Path,
        plugins: &Path,
        dir: &Path,
        ssh: &Path,
        args: &[&str],
    ) -> (bool, String) {
        let properties = dir.join("gateway.properties");
        std::fs::write(
            &properties,
            format!(
                "idea.config.path={0}/config\nidea.system.path={0}/system\nidea.log.path={0}/log\nidea.plugins.path={1}\n",
                dir.display(),
                plugins.display()
            ),
        )
        .unwrap();
        let options = dir.join("gateway.vmoptions");
        std::fs::copy(gateway_dir.join("bin/gateway64.vmoptions"), &options).unwrap();
        let log = dir.join("launcher.out");
        let mut child = std::process::Command::new(gateway_dir.join("bin/gateway.sh"))
            .args(args)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env("GATEWAY_PROPERTIES", &properties)
            .env("GATEWAY_VM_OPTIONS", &options)
            .env("PUDDLE_PROBE_SSH", ssh)
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::process::Stdio::from(
                std::fs::File::create(dir.join("launcher.err")).unwrap(),
            ))
            .spawn()
            .expect("Gateway's launcher runs");
        // Well under the runner's limit, so a hung step still leaves its report and logs here.
        let deadline = std::time::Instant::now() + PROBE_LIMIT;
        let ok = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status.success();
            }
            if std::time::Instant::now() > deadline {
                // Gateway's launcher execs java, so the child is Gateway itself: its threads first.
                if let Ok(dump) = std::process::Command::new(gateway_dir.join("jbr/bin/jstack"))
                    .arg(child.id().to_string())
                    .output()
                {
                    let _ = std::fs::write(dir.join("launcher.err.jstack"), &dump.stdout);
                    eprintln!("--- jstack\n{}", String::from_utf8_lossy(&dump.stdout));
                }
                let _ = child.kill();
                let _ = child.wait();
                eprintln!("gateway probe: killed after {} s", PROBE_LIMIT.as_secs());
                break false;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        let text = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        (
            ok,
            format!("{}{}", text("launcher.out"), text("launcher.err")),
        )
    }

    async fn run() {
        let gateway_dir = env_dir("PUDDLE_GATEWAY_DIR");
        let scratch = tempfile::tempdir().unwrap();
        let plugins = build_probe(&gateway_dir, scratch.path());
        let mut options: HostOptions = named_id_options();
        options.launcher = Arc::new(VsCodeLauncher::with_program(Path::new("/bin/true")));
        let user_config = scratch.path().join("home/.ssh/config");
        let uc = user_config.clone();
        let exe = std::env::current_exe().unwrap();
        let VmHost {
            host,
            root,
            settings,
            ..
        } = start_vm_host_config("gateway", options, move |config| {
            config.ssh.bridge_program = Some(exe);
            config.ssh.user_config = Some(uc);
        })
        .await;
        let name = format!("{}-gateway", settings.prefix.as_str());
        let workspace = WorkspaceId::new(&name).unwrap();
        let api = Api::new(host.url(), host.token());
        let mut events = api.events().await;
        let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
        let reply = api
            .post(
                "/api/workspaces",
                &json!({"name": name, "repositories": [], "memory_mib": 4096}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events
            .until(ended(name_static), Duration::from_secs(600))
            .await;
        assert_eq!(end["step"], "done", "{end}");
        // Gateway's bootstrap warns below four cores (its "minimal requirements" check; two still
        // warned), so the workspace gets four, as a user would set it, and starts again to take them.
        let reply = api
            .put(
                &format!("/api/settings/workspaces/{name}"),
                &json!({"overrides": {"direct_ssh": true, "cpus": 4}}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        api.post(&format!("/api/workspaces/{name}/stop"), "").await;
        events
            .until(ended(name_static), Duration::from_secs(120))
            .await;
        api.post(&format!("/api/workspaces/{name}/start"), "").await;
        let end = events
            .until(ended(name_static), Duration::from_secs(300))
            .await;
        assert_eq!(end["step"], "done", "{end}");
        assert!(host.workspaces().ssh_endpoint(&workspace).await.is_some());

        // OpenSSH reads ~/.ssh/config from the passwd entry, not from $HOME, so Gateway's own
        // "ssh executable" setting points at a wrapper that names the user's file the test owns,
        // the one with Puddle's include line.
        let ssh = scratch.path().join("ssh");
        puddle_test_kit::write_executable(
            &ssh,
            &format!(
                "#!/bin/sh\nexec ssh -F '{}' \"$@\"\n",
                user_config.display()
            ),
        );
        let report = scratch.path().join("report.txt");
        let alias = format!("puddle-{name}");
        let started = std::time::Instant::now();
        let (ok, output) = tokio::task::spawn_blocking({
            let report = report.clone();
            let dir = scratch.path().join("gateway");
            let alias = alias.clone();
            move || {
                std::fs::create_dir_all(&dir).unwrap();
                gateway(
                    &gateway_dir,
                    &plugins,
                    &dir,
                    &ssh,
                    &[
                        "puddleinspect",
                        &alias,
                        report.to_str().unwrap(),
                        GUEST_PORT,
                        "ssh,deploy",
                    ],
                )
            }
        })
        .await
        .unwrap();
        let report_text = std::fs::read_to_string(&report).unwrap_or_default();
        eprintln!(
            "gateway probe: {} s\n--- report\n{report_text}\n--- launcher output (tail)\n{}",
            started.elapsed().as_secs(),
            output
                .lines()
                .rev()
                .take(60)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        );
        if !ok || !report_text.contains("DONE failures=0") {
            let log = scratch.path().join("gateway/log/idea.log");
            if let Ok(log) = std::fs::read_to_string(&log) {
                eprintln!(
                    "--- idea.log (tail)\n{}",
                    log.lines()
                        .rev()
                        .take(200)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            }
        }
        assert!(
            ok && report_text.contains("DONE failures=0"),
            "the probe failed, see the report above"
        );
        // Gateway accepted the new host key itself (`StrictHostKeyChecking accept-new`) and wrote it
        // where Puddle's entry says.
        let known =
            std::fs::read_to_string(root.join("ssh").join("known_hosts").join(&name)).unwrap();
        assert!(known.contains(&alias), "{known}");
        host.shutdown().await;
    }
}
