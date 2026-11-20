// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bundled code-server installs an extension in a fresh workspace on the first try, with no
//! rule added by anyone (K on Linux KVM): System managed allows Open VSX for every workspace from
//! the start (rules spec R-41), whether or not that workspace's browser editor was ever opened.
//!
//! The install runs the editor server's own command line and environment (read from its running
//! process), as the user it runs as, so the traffic is the one a click on "Install" makes: through
//! the workspace's proxy settings and CA, to the live Open VSX registry. The registry is a live
//! third party on purpose: only it can say whether the hosts the allow list names are the hosts an
//! install uses (the file download is a redirect to a second host).
//!
//! Linux only: the pinned code-server release is a Linux build (`PUDDLE_CODE_SERVER_DIR`).
#![cfg(target_os = "linux")]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime as _, Sandbox as _};
use puddle_e2e::lab::{Squid, SquidOptions};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::{Api, ended, named_id_options};
use vm_support::{
    VmHost, guest, guest_live, guest_within, start_vm_host_config, through_company_proxy_port,
};

/// A small extension that is on Open VSX.
const EXTENSION: &str = "EditorConfig.EditorConfig";

/// The extensions a Python and a JavaScript developer install first.
const POPULAR: [&str; 2] = ["ms-python.python", "dbaeumer.vscode-eslint"];

/// The hosts that wait for approval in the Inbox after installing [`POPULAR`] in a fresh workspace:
/// everything the installs use beyond Open VSX, which System managed already allows. A new host here
/// is an extension that started to need one more approval (every user's first install meets it); an
/// empty list that stops being empty fails the same way.
const POPULAR_PENDING_HOSTS: &[&str] = &[];

/// The hosts an install uses: the registry's API, and its file host (the download redirects there).
const REGISTRY: &str = "open-vsx.org openvsx.eclipsecontent.org";

/// Waits until the editor server answers its health check.
const EDITOR_UP: &str = "for i in $(seq 1 100); do curl -sf -o /dev/null http://127.0.0.1:41337/healthz && exit 0; sleep 1; done; exit 1";

/// Installs [`EXTENSION`] the way the running editor server would: its program, its environment
/// and its folders, as its user. Prints what the install said and what the extensions folder holds.
const INSTALL: &str = r#"
pid=$(pgrep -f -o -- '--extensions-dir')
[ -n "$pid" ] || { echo "no editor server process"; exit 1; }
user=$(stat -c %U "/proc/$pid")
exts=$(tr '\0' '\n' <"/proc/$pid/cmdline" | grep -A1 -x -- '--extensions-dir' | tail -n 1)
data=$(tr '\0' '\n' <"/proc/$pid/cmdline" | grep -A1 -x -- '--user-data-dir' | tail -n 1)
prog=$(find / -maxdepth 5 -path "*/bin/code-server" -not -path "/proc/*" 2>/dev/null | head -n 1)
[ -x "$prog" ] || { echo "no code-server program"; exit 1; }
env_file=$(mktemp)
tr '\0' '\n' <"/proc/$pid/environ" | grep -E '^(HTTP|HTTPS|NO|ALL)_PROXY=|^(http|https|no|all)_proxy=|^NODE_|^SSL_CERT|^HOME=|^PATH=' >"$env_file"
chmod 644 "$env_file"
echo "user=$user extensions=$exts"
runuser -u "$user" -- bash -c '
  set -a; . "$1"; set +a
  cd "$HOME" 2>/dev/null || cd /
  "$5" --install-extension "$2" --extensions-dir "$3" --user-data-dir "$4" 2>&1
' _ "$env_file" "$EXTENSION" "$exts" "$data" "$prog"
status=$?
echo "install exit=$status"
ls -1 "$exts"
exit $status
"#;

/// The popular extensions install in a fresh workspace and ask for exactly the pinned hosts. The
/// installs go to the live Open VSX and whatever else each extension downloads, so the test is a
/// radar (weekly `full-test`, never a merge gate): it discovers an extension that began to need a
/// host nobody pinned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_editor_extensions_python_and_eslint_ask_only_for_the_pinned_hosts() {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let vm = start_vm_host_config("pop", named_id_options(), |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-pop", vm.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [], "memory_mib": 2048}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    guest(&vm.runtime, &sandbox, EDITOR_UP).await;
    assert_eq!(api.get("/api/rules").await.json()["rules"], json!([]));
    for extension in POPULAR {
        let script = format!("EXTENSION={extension}\nexport EXTENSION\n{INSTALL}");
        let out = guest_live(&vm.runtime, &sandbox, &script, "open-vsx.org").await;
        eprintln!("{extension}:\n{out}");
        assert!(
            out.to_lowercase().contains("successfully installed"),
            "{extension} did not report success:\n{out}"
        );
        let folder = extension.to_lowercase();
        assert!(
            out.lines()
                .any(|l| l.to_lowercase().starts_with(&format!("{folder}-"))),
            "{extension} is not in the extensions folder:\n{out}"
        );
    }
    // A refusal's row is written when the connection is; let the audit and the inbox settle.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let pending = api
        .get(&format!("/api/pending?workspace={name}"))
        .await
        .json();
    let mut hosts: Vec<String> = pending["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["host"].as_str().unwrap().to_owned())
        .collect();
    hosts.sort();
    hosts.dedup();
    eprintln!("hosts waiting for approval: {hosts:?}");
    let mut pinned: Vec<String> = POPULAR_PENDING_HOSTS
        .iter()
        .map(|h| (*h).to_owned())
        .collect();
    pinned.sort();
    assert_eq!(
        hosts, pinned,
        "the hosts waiting for approval after installing {POPULAR:?} moved"
    );
    assert_eq!(api.get("/api/rules").await.json()["rules"], json!([]));
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_editor_installs_an_extension_from_open_vsx_with_no_rule_added() {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_config("ovx", named_id_options(), |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-ovx", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(host.url(), host.token());

    // A fresh install: no rule of the user's own, no rule set switched on, the default editor
    // server, and Open VSX already listed with its reason before any workspace exists.
    assert_eq!(api.get("/api/rules").await.json()["rules"], json!([]));
    let listed = api.get("/api/rule-sets").await.json();
    let system: Vec<(&str, &str)> = listed["system_managed"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| Some((row["pattern"].as_str()?, row["reason"].as_str()?)))
        .collect();
    for host_name in REGISTRY.split(' ') {
        assert!(
            system.contains(&(host_name, "code_server")),
            "{host_name} is not System managed on a fresh install: {listed}"
        );
    }

    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [], "memory_mib": 2048}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    guest(&runtime, &sandbox, EDITOR_UP).await;

    // First use: the install goes through, and nothing was asked of the user.
    let script = format!("EXTENSION={EXTENSION}\nexport EXTENSION\n{INSTALL}");
    let out = guest_live(&runtime, &sandbox, &script, REGISTRY).await;
    eprintln!("install output:\n{out}");
    assert!(
        out.to_lowercase().contains("successfully installed"),
        "the install did not report success:\n{out}"
    );
    assert!(
        out.lines()
            .any(|l| l.to_lowercase().starts_with("editorconfig.editorconfig-")),
        "the extension is not in the extensions folder:\n{out}"
    );

    // Both hosts were reached on the allow of System managed, with no pending request and no
    // rule created on the way.
    for host_name in REGISTRY.split(' ') {
        // The audit log is written a moment after the connection, so wait for the record.
        let entries = puddle_test_kit::eventually_async(
            "a connection record",
            Duration::from_secs(30),
            || async {
                let page = api
                    .get(&format!(
                        "/api/audit?workspace={name}&type=connection&host_contains={host_name}&limit=100"
                    ))
                    .await
                    .json();
                let entries = page["entries"].as_array().unwrap().clone();
                if entries.is_empty() {
                    Err(format!("no connection to {host_name} recorded yet"))
                } else {
                    Ok(entries)
                }
            },
        )
        .await;
        for entry in entries {
            let record = &entry["record"];
            assert_eq!(record["decision"], "allow", "{record}");
            assert_eq!(record["rule_set"], "system", "{record}");
            assert_eq!(record["pending_id"], Value::Null, "{record}");
        }
    }
    assert_eq!(api.get("/api/rules").await.json()["rules"], json!([]));
    let pending = api.get("/api/pending").await.json();
    assert_eq!(pending["requests"], json!([]), "{pending}");

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// What the tools of [`vm_radar_company_squid_carries_pip_a_build_and_an_open_vsx_install`] run as
/// the workspace's user in a login shell: a wheel from `PyPI` into a virtual environment, then a
/// build from source (the sdist, and the build backend it fetches into its isolated build
/// environment).
const PIP_AND_A_BUILD: &str = r#"
set -e
python3 -m venv ~/venv
~/venv/bin/pip install --no-cache-dir -q six
~/venv/bin/python -c 'import six; print("wheel-ok")'
~/venv/bin/pip install --no-cache-dir --no-binary :all: --force-reinstall -q six
~/venv/bin/python -c 'import six; print("source-build-ok")'
"#;

/// The real tools of a day's work run behind a real Squid that is the host's company proxy:
/// `pip` (a wheel), a build from source (an sdist and its build backend) and the editor's
/// extension install from Open VSX all reach their hosts, and Squid's own access log is the
/// witness that every one of them went through it. The tools reach live third parties (`PyPI`,
/// Open VSX), so this is a radar: an outage there fails it, a per-pull-request job skips it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_company_squid_carries_pip_a_build_and_an_open_vsx_install() {
    let Some(squid) = Squid::start(&SquidOptions::open(&[80, 443]).live()) else {
        return;
    };
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let options = through_company_proxy_port(named_id_options(), squid.port());
    let vm = start_vm_host_config("sqd", options, |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-sqd", vm.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(vm.host.url(), vm.host.token());
    for host in [
        "deb.debian.org",
        "security.debian.org",
        "pypi.org",
        "files.pythonhosted.org",
    ] {
        let rule = api
            .post(
                "/api/rules",
                &json!({"scope": {"type": "global"}, "pattern": host, "effect": "allow"})
                    .to_string(),
            )
            .await;
        assert_eq!(rule.status, 201, "{}", rule.body);
    }
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [], "memory_mib": 2048}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    guest(&vm.runtime, &sandbox, EDITOR_UP).await;

    // The image ships no Python: Debian's through apt is the first thing that goes through Squid.
    guest_within(
        &vm.runtime,
        &sandbox,
        "apt-get update -qq && apt-get install -y -qq --no-install-recommends python3 python3-pip python3-venv ca-certificates >/tmp/apt.log 2>&1 || { cat /tmp/apt.log; exit 1; }",
        Duration::from_mins(5),
    )
    .await;
    let handle = vm.runtime.get(&sandbox.sandbox_name()).await.unwrap();
    // As the workspace's user in a login shell, the way a terminal of the workspace runs it.
    let ran = handle
        .exec(
            ExecRequest::sh(format!(
                "cd && exec bash -l -c '{}'",
                PIP_AND_A_BUILD.replace('\'', "'\\''")
            ))
            .as_user("agent")
            .with_timeout(Duration::from_mins(10)),
        )
        .await
        .unwrap();
    let python = format!("{}{}", ran.stdout_text(), ran.stderr_text());
    assert_eq!(ran.status.code, 0, "{python}");
    assert!(
        python.contains("wheel-ok") && python.contains("source-build-ok"),
        "{python}"
    );
    let script = format!("EXTENSION={EXTENSION}\nexport EXTENSION\n{INSTALL}");
    let out = guest_live(&vm.runtime, &sandbox, &script, REGISTRY).await;
    assert!(
        out.to_lowercase().contains("successfully installed"),
        "the install did not report success:\n{out}"
    );

    // Squid writes a tunnel's line when the tunnel ends, so each wait is for its own host.
    for host in [
        "pypi.org:443",
        "files.pythonhosted.org:443",
        "open-vsx.org:443",
    ] {
        let log = squid.access_log_with(host);
        assert!(
            log.lines()
                .any(|line| line.contains("TCP_TUNNEL/200") && line.contains(host)),
            "Squid carried nothing to {host}:\n{log}"
        );
    }
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
