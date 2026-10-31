// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop attach end to end on a real microVM (K on Linux KVM): a real OpenSSH `ssh` logs in to a
//! workspace through the ssh config entry the host wrote, the bridge, the endpoint and the
//! runtime's own SSH server, with the login key the host made.
//!
//! This binary is its own `ProxyCommand`: the entry names it as the bridge program and it relays
//! as `puddle ssh-bridge` does when it is started with `ssh-bridge <endpoint>` (the real bridge
//! has its own tests against OpenSSH's `sshd -i`). It has no test harness for that reason, and
//! answers the runner's `--list` itself.
//!
//! What the hostile guest part shows: the login key's private half never reaches the guest and
//! the guest has no SSH daemon of its own; another key is refused by the runtime's server; a host
//! key a workspace had before it was rebuilt does not stop the next login; with the switch off
//! the name does not resolve and the old endpoint says so.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a plain test program for a VM: a failed step fails the run; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::process::ExitCode;

type Case = (&'static str, fn());

#[cfg(unix)]
const CASES: &[Case] = &[(
    "vm_desktop_attach_logs_in_with_the_hosts_key_and_nothing_else",
    unix::logs_in_with_the_hosts_key_and_nothing_else,
)];

#[cfg(not(unix))]
const CASES: &[Case] = &[];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("ssh-bridge") {
        return bridge(args.get(1).map(String::as_str));
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

/// What `puddle ssh-bridge <endpoint>` does: relay standard input and output to the endpoint.
fn bridge(endpoint: Option<&str>) -> ExitCode {
    let Some(endpoint) = endpoint else {
        eprintln!("puddle: missing endpoint");
        return ExitCode::from(2);
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
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("puddle: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
#[expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "a plain test program for a VM: a failed step fails the run"
)]
mod unix {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Output;
    use std::sync::Arc;
    use std::time::Duration;

    use puddle_host::{HostOptions, VsCodeLauncher};
    use puddle_types::WorkspaceName;
    use serde_json::json;

    use crate::support::{Api, ended, hermetic_options};
    use crate::vm_support::{VmHost, guest, start_vm_host_config};

    const WAIT: Duration = Duration::from_secs(120);

    /// Runs `ssh` with `args`, failing the test when it takes longer than [`WAIT`].
    async fn ssh(args: &[&str]) -> Output {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let run = tokio::task::spawn_blocking(move || {
            std::process::Command::new("ssh")
                .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=60"])
                .args(&args)
                .output()
                .expect("OpenSSH's ssh runs (a requirement of this test)")
        });
        tokio::time::timeout(WAIT, run)
            .await
            .expect("ssh finished in time")
            .unwrap()
    }

    fn text(out: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    }

    fn keygen(path: &Path) -> String {
        let out = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(path)
            .output()
            .expect("ssh-keygen runs");
        assert!(out.status.success(), "{}", text(&out));
        std::fs::read_to_string(path.with_extension("pub"))
            .unwrap()
            .trim()
            .to_owned()
    }

    pub(super) fn logs_in_with_the_hosts_key_and_nothing_else() {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap()
            .block_on(run());
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one VM, one story, read top to bottom"
    )]
    async fn run() {
        let scratch = tempfile::tempdir().unwrap();
        let bin = scratch.path().join("code");
        let seen = scratch.path().join("code-args");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", seen.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut options: HostOptions = hermetic_options();
        options.launcher = Arc::new(VsCodeLauncher::with_program(&bin));
        let user_config = scratch.path().join("home/.ssh/config");
        let uc = user_config.clone();
        let exe = std::env::current_exe().unwrap();
        let VmHost {
            host,
            runtime,
            settings,
            root,
            ..
        } = start_vm_host_config("dattach", options, move |config| {
            config.ssh.bridge_program = Some(exe);
            config.ssh.user_config = Some(uc);
        })
        .await;
        let name = format!("{}-dattach", settings.prefix.as_str());
        let workspace = WorkspaceName::new(&name).unwrap();
        let sandbox = workspace.clone();
        let api = Api::new(host.url(), host.token());
        let mut events = api.events().await;
        let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
        let reply = api
            .post(
                "/api/workspaces",
                &json!({"name": name, "repositories": [], "memory_mib": 1024}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events
            .until(ended(name_static), Duration::from_secs(600))
            .await;
        assert_eq!(end["step"], "done", "{end}");

        let ssh_dir = root.join("ssh");
        let config = ssh_dir.join("config");
        let alias = format!("puddle-{name}");
        let cfg = config.to_str().unwrap().to_owned();
        let direct = |on: bool| {
            let api = &api;
            let name = &name;
            async move {
                let reply = api
                    .put(
                        &format!("/api/settings/workspaces/{name}"),
                        &json!({"overrides": {"direct_ssh": on}}).to_string(),
                    )
                    .await;
                assert_eq!(reply.status, 200, "{}", reply.body);
            }
        };

        // Off: no entry, no endpoint, nothing for the name to resolve to.
        assert!(host.workspaces().ssh_endpoint(&workspace).await.is_none());
        // (No file of Puddle's exists yet, so nothing defines the name.)
        let off = ssh(&["-F", "/dev/null", &alias, "true"]).await;
        assert!(!off.status.success());
        assert!(
            text(&off).contains("Could not resolve hostname"),
            "{}",
            text(&off)
        );

        // On: ssh logs in as root through the entry, with Puddle's own file and through the
        // user's file's include line.
        direct(true).await;
        let first_endpoint = host.workspaces().ssh_endpoint(&workspace).await.unwrap();
        let login = ssh(&["-F", &cfg, &alias, "id -un; echo from-the-guest"]).await;
        assert!(login.status.success(), "{}", text(&login));
        let said = String::from_utf8_lossy(&login.stdout).into_owned();
        assert!(
            said.starts_with("root\n") && said.contains("from-the-guest"),
            "{said}"
        );
        let uc = user_config.to_str().unwrap();
        let through_include = ssh(&["-F", uc, &alias, "hostname"]).await;
        assert!(
            through_include.status.success(),
            "{}",
            text(&through_include)
        );

        // The editor is opened on the entry's host.
        let attach = api
            .post(
                &format!("/api/workspaces/{name}/attach"),
                &json!({"mode": "desktop"}).to_string(),
            )
            .await;
        assert_eq!(attach.json()["opened"], true, "{}", attach.body);
        let args = std::fs::read_to_string(&seen).unwrap();
        assert!(
            args.starts_with("--folder-uri\nvscode-remote://ssh-remote+")
                && args.contains(&format!("+{alias}/workspaces/")),
            "{args}"
        );

        // Hostile guest: the private key is nowhere in the guest, and the guest has no SSH daemon.
        let found = guest(
            &runtime,
            &sandbox,
            "grep -rIl 'BEGIN OPENSSH PRIVATE' /root /etc /home /tmp /run /workspaces /opt /var 2>/dev/null; echo done",
        )
        .await;
        assert_eq!(
            found.trim(),
            "done",
            "a private key is in the guest: {found}"
        );
        let listening = guest(&runtime, &sandbox, "cat /proc/net/tcp /proc/net/tcp6 2>/dev/null | awk 'NR>1 && $4==\"0A\" {print $2}'; echo done").await;
        assert!(
            !listening.contains(":0016"),
            "something listens on port 22 in the guest: {listening}"
        );

        // Another key is refused by the runtime's server; the host's own key is not.
        let other = scratch.path().join("other");
        let other_pub = keygen(&other);
        let proxy = format!(
            "ProxyCommand=\"{}\" ssh-bridge \"{}\"",
            std::env::current_exe().unwrap().display(),
            first_endpoint.display()
        );
        let bare = |identity: &Path| {
            let identity = identity.display().to_string();
            let proxy = proxy.clone();
            async move {
                ssh(&[
                    "-F",
                    "/dev/null",
                    "-o",
                    &proxy,
                    "-o",
                    &format!("IdentityFile={identity}"),
                    "-o",
                    "IdentitiesOnly=yes",
                    "-o",
                    "IdentityAgent=none",
                    "-o",
                    "UserKnownHostsFile=/dev/null",
                    "-o",
                    "StrictHostKeyChecking=no",
                    "root@x",
                    "true",
                ])
                .await
            }
        };
        let refused = bare(&other).await;
        assert_eq!(refused.status.code(), Some(255), "{}", text(&refused));
        assert!(
            text(&refused).contains("Permission denied"),
            "{}",
            text(&refused)
        );
        let ours = bare(&ssh_dir.join("login_ed25519")).await;
        assert!(ours.status.success(), "control: {}", text(&ours));

        // A host key left over from before the sandbox was made anew stops ssh until the host
        // forgets it, which it does when the endpoint opens.
        let known = ssh_dir.join("known_hosts").join(&name);
        std::fs::write(&known, format!("{alias} {other_pub}\n")).unwrap();
        let stale = ssh(&["-F", &cfg, &alias, "true"]).await;
        assert_eq!(stale.status.code(), Some(255), "control: {}", text(&stale));
        assert!(
            text(&stale).contains("HOST IDENTIFICATION HAS CHANGED"),
            "{}",
            text(&stale)
        );
        api.post(&format!("/api/workspaces/{name}/stop"), "").await;
        events
            .until(ended(name_static), Duration::from_secs(120))
            .await;
        let config_text = std::fs::read_to_string(&config).unwrap();
        assert!(
            !config_text.contains("Host puddle-"),
            "a stopped workspace has no entry"
        );
        api.post(&format!("/api/workspaces/{name}/start"), "").await;
        events
            .until(ended(name_static), Duration::from_secs(300))
            .await;
        let second_endpoint = host.workspaces().ssh_endpoint(&workspace).await.unwrap();
        assert_ne!(first_endpoint, second_endpoint);
        let again = ssh(&["-F", &cfg, &alias, "echo again"]).await;
        assert!(again.status.success(), "{}", text(&again));

        // Off again: the entry goes, and the endpoint's old name says why it is gone.
        direct(false).await;
        assert!(
            !std::fs::read_to_string(&config)
                .unwrap()
                .contains("Host puddle-")
        );
        let gone = ssh(&["-F", &cfg, &alias, "true"]).await;
        assert!(
            text(&gone).contains("Could not resolve hostname"),
            "{}",
            text(&gone)
        );
        let old = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("ssh-bridge")
            .arg(&second_endpoint)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!old.status.success());
        assert!(
            String::from_utf8_lossy(&old.stderr).contains("direct SSH is off for it"),
            "{}",
            text(&old)
        );
        let refused = api
            .post(
                &format!("/api/workspaces/{name}/attach"),
                &json!({"mode": "desktop"}).to_string(),
            )
            .await;
        assert_eq!(refused.status, 409, "{}", refused.body);

        // The include line is in the user's file once, and nothing else is.
        let user = std::fs::read_to_string(&user_config).unwrap();
        assert_eq!(user.lines().count(), 1, "{user}");
        assert_eq!(user.matches("Include").count(), 1);

        let _: PathBuf = second_endpoint;
        host.shutdown().await;
    }
}
