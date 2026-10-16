// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate on a real microVM (tier K on Linux KVM, W on Windows WHP).
//!
//! The guest gets the certificate, its key and the step exactly as the boot plan carries them
//! (the files of `GuestTrust::with_dev_cert`, applied with the plan's own modes, then
//! `update-ca-certificates`, then the steps in plan order). Two images:
//!
//! - `node:22-bookworm` has curl, Node and openssl. An `openssl s_server` that holds only the
//!   certificate and key stands in for a dev server; curl, Node and the system store must accept it
//!   **without `-k`**, including for `*.dev.localhost` and `127.0.0.1`. After a renewed certificate
//!   is applied (a re-boot) the new one is trusted and the old one is not.
//! - `mcr.microsoft.com/dotnet/sdk:10.0` has .NET: `dotnet dev-certs https --check --trust` passes
//!   for the imported ECDSA certificate and Kestrel serves exactly that certificate, which curl
//!   trusts; the renewed one replaces it at the next boot.
//!
//! The steps are applied by plain exec rather than through `boot.sh`, as in `vm_root_sync.rs`
//! (on a real msb VM `boot.sh` stops at its first sysctl); the fake-root test
//! `puddle-certs/tests/boot_trust.rs` covers the same plan through `boot.sh`.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code outside #[test] fns: a failed check fails the test, output goes to the log"
)]

use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use microsandbox::Sandbox;
use puddle_ca::TrustBundle;
use puddle_certs::devcert::{DevCertManager, GuestDevCert, Unsupported};
use puddle_certs::{CorporateRoots, GuestTrust};
use puddle_vm_tests::{HarnessError, VmEnv, within};

/// Has curl, Node 22 and openssl, runs as root.
const NODE_IMAGE: &str = "node:22-bookworm";
/// Has .NET 10, curl and (checked by the test) openssl.
const DOTNET_IMAGE: &str = "mcr.microsoft.com/dotnet/sdk:10.0";
const CREATE_BUDGET: Duration = Duration::from_mins(15);
const EXEC_BUDGET: Duration = Duration::from_mins(2);
const BUILD_BUDGET: Duration = Duration::from_mins(7);
const STOP_BUDGET: Duration = Duration::from_mins(1);

/// A fresh development certificate and its trust, as a host would give one workspace.
fn trust() -> GuestTrust {
    let dir = tempfile::tempdir().unwrap();
    let report = DevCertManager::new(dir.path(), Unsupported)
        .ensure(SystemTime::now())
        .unwrap();
    GuestTrust::new(&CorporateRoots::default(), &TrustBundle::new())
        .with_dev_cert(GuestDevCert::new(&report.material))
}

/// The uploads (`/tmp/devcert-<n>`, then the env file) and the script that does what `boot.sh`
/// does with this trust: write each file with its mode, the env file, `update-ca-certificates`
/// (a CA file changed) and the plan's steps, in order.
fn install(trust: &GuestTrust) -> (Vec<(String, Vec<u8>)>, String) {
    let mut uploads = Vec::new();
    let mut script = String::from("set -e\n");
    for (n, f) in trust.guest_files().iter().enumerate() {
        let tmp = format!("/tmp/devcert-{n}");
        let dest = f.path().as_str();
        let dir = dest.rsplit_once('/').unwrap().0;
        writeln!(
            script,
            "mkdir -p '{dir}' && cp '{tmp}' '{dest}' && chmod {:o} '{dest}'",
            f.mode()
        )
        .unwrap();
        uploads.push((tmp, f.contents().to_vec()));
    }
    let mut env = String::new();
    for (k, v) in trust.env().iter() {
        writeln!(env, "export {k}='{v}'").unwrap();
    }
    uploads.push(("/tmp/devcert-env".to_owned(), env.into_bytes()));
    script.push_str("cp /tmp/devcert-env /etc/profile.d/01-puddle-env.sh\n");
    script.push_str("export HOME=/root DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1\n");
    script.push_str("update-ca-certificates --fresh >/dev/null\n");
    for step in trust.boot_steps() {
        writeln!(script, "echo \"step {step}: $(sh {step})\"").unwrap();
    }
    (uploads, script)
}

async fn sh(
    sandbox: &Sandbox,
    what: &'static str,
    budget: Duration,
    script: String,
) -> Result<(i32, String), HarnessError> {
    let out = within(what, budget, sandbox.shell(script)).await??;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(out.stdout_bytes()),
        String::from_utf8_lossy(out.stderr_bytes())
    );
    Ok((out.status().code, text))
}

/// Uploads and applies `trust`; returns what the steps said.
async fn apply(sandbox: &Sandbox, trust: &GuestTrust) -> Result<String, HarnessError> {
    let (uploads, script) = install(trust);
    let fs = sandbox.fs();
    for (path, data) in uploads {
        within("write", EXEC_BUDGET, fs.write(&path, data)).await??;
    }
    let (code, out) = sh(sandbox, "apply", BUILD_BUDGET, script).await?;
    eprintln!("applied:\n{out}");
    assert_eq!(code, 0, "apply failed: {out}");
    Ok(out)
}

/// Starts an `openssl s_server` on 8443 with the certificate and key at the given guest paths,
/// waits for it, runs `clients` with the plan's env sourced, then stops it.
fn with_s_server(cert: &str, key: &str, clients: &str) -> String {
    format!(
        "set -u
openssl s_server -accept 8443 -cert {cert} -key {key} -www -quiet >/tmp/s_server.log 2>&1 &
srv=$!
i=0
until python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", 8443), 1)' 2>/dev/null; do
  i=$((i+1)); [ $i -gt 100 ] && {{ echo 'server did not start'; cat /tmp/s_server.log; kill $srv; exit 99; }}
  sleep 0.1
done
# Before the boot files there is no env file, and `.` of a missing file ends a POSIX shell.
if [ -f /etc/profile.d/01-puddle-env.sh ]; then . /etc/profile.d/01-puddle-env.sh; fi
{clients}
rc=$?
kill $srv 2>/dev/null
exit $rc"
    )
}

const NODE_GET: &str = "node -e \"require('https').get('https://localhost:8443/', r => { console.log(r.statusCode); r.resume(); }).on('error', e => { console.log(e.message); process.exit(1); })\"";

fn clients() -> String {
    format!(
        "echo \"curl: $(curl -sS -o /dev/null -w '%{{http_code}}' https://localhost:8443/)\"
echo \"curl-system: $(env -u CURL_CA_BUNDLE -u SSL_CERT_FILE -u SSL_CERT_DIR curl -sS -o /dev/null -w '%{{http_code}}' https://localhost:8443/)\"
echo \"curl-wildcard: $(curl -sS -o /dev/null -w '%{{http_code}}' --resolve app.dev.localhost:8443:127.0.0.1 https://app.dev.localhost:8443/)\"
echo \"curl-ip: $(curl -sS -o /dev/null -w '%{{http_code}}' https://127.0.0.1:8443/)\"
echo \"node: $({NODE_GET})\"
echo \"vite-style: $(test -r \"$DEV_CERT_PEM\" && test -r \"$DEV_KEY_PEM\" && echo readable)\""
    )
}

async fn node_run(env: &VmEnv) -> Result<(), HarnessError> {
    let builder = env.sandbox("devcert-node", NODE_IMAGE)?;
    env.scope(Box::pin(async {
        let sandbox = within("create", CREATE_BUDGET, builder.create()).await??;
        let first = trust();
        let file = |t: &GuestTrust, p: &str| {
            t.guest_files()
                .iter()
                .find(|f| f.path().as_str() == p)
                .unwrap()
                .contents()
                .to_vec()
        };
        let fs = sandbox.fs();
        // The fixture server holds the certificate and key from a place no trust looks at.
        for (name, p) in [("a.crt", "/etc/puddle/dev-cert/localhost.crt"), ("a.key", "/etc/puddle/dev-cert/localhost.key")] {
            within("write", EXEC_BUDGET, fs.write(&format!("/tmp/{name}"), file(&first, p))).await??;
        }

        // Negative control: the image alone does not trust the certificate.
        let (code, out) = sh(
            &sandbox,
            "curl before",
            EXEC_BUDGET,
            with_s_server("/tmp/a.crt", "/tmp/a.key", "curl -sS https://localhost:8443/"),
        )
        .await?;
        assert_ne!(code, 0, "curl trusted the certificate before the boot files: {out}");

        let steps = apply(&sandbox, &first).await?;
        // No .NET in this image: the step says so, and the boot carries on.
        assert!(steps.contains("certificate in place; no .NET SDK in this image"), "{steps}");
        let (code, out) = sh(
            &sandbox,
            "clients",
            EXEC_BUDGET,
            with_s_server("/etc/puddle/dev-cert/localhost.crt", "/etc/puddle/dev-cert/localhost.key", &clients()),
        )
        .await?;
        eprintln!("clients after the boot files:\n{out}");
        assert_eq!(code, 0, "{out}");
        for line in [
            "curl: 200",
            "curl-system: 200",
            "curl-wildcard: 200",
            "curl-ip: 200",
            "node: 200",
            "vite-style: readable",
        ] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in:\n{out}");
        }
        let (code, out) = sh(
            &sandbox,
            "key mode",
            EXEC_BUDGET,
            "stat -c '%a %U' /etc/puddle/dev-cert/localhost.key".to_owned(),
        )
        .await?;
        assert_eq!((code, out.trim()), (0, "600 root"));

        // A re-boot with a renewed certificate: the new one is trusted, the old one is not.
        apply(&sandbox, &trust()).await?;
        let (code, out) = sh(
            &sandbox,
            "clients renewed",
            EXEC_BUDGET,
            with_s_server("/etc/puddle/dev-cert/localhost.crt", "/etc/puddle/dev-cert/localhost.key", &clients()),
        )
        .await?;
        assert_eq!(code, 0, "{out}");
        assert!(out.lines().any(|l| l == "curl-system: 200"), "{out}");
        let (code, out) = sh(
            &sandbox,
            "old certificate",
            EXEC_BUDGET,
            with_s_server(
                "/tmp/a.crt",
                "/tmp/a.key",
                "curl -sS -o /dev/null https://localhost:8443/; echo \"old: $?\"; env -u CURL_CA_BUNDLE -u SSL_CERT_FILE -u SSL_CERT_DIR curl -sS -o /dev/null https://localhost:8443/; echo \"old-system: $?\"; true",
            ),
        )
        .await?;
        assert_eq!(code, 0, "{out}");
        for line in out.lines().filter(|l| l.starts_with("old")) {
            assert!(!line.ends_with(": 0"), "the replaced certificate is still trusted: {out}");
        }
        assert_eq!(out.lines().filter(|l| l.starts_with("old")).count(), 2, "{out}");

        within("stop", STOP_BUDGET, sandbox.stop()).await??;
        within("remove", STOP_BUDGET, Sandbox::remove(sandbox.name())).await??;
        Ok(())
    }))
    .await
}

/// Starts the `web` template app (Kestrel) on `https://localhost:7443`, runs `body` against it,
/// stops it.
fn with_kestrel(body: &str) -> String {
    format!(
        "set -u
. /etc/profile.d/01-puddle-env.sh
export HOME=/root DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 ASPNETCORE_URLS=https://localhost:7443
cd /tmp/app
dotnet bin/Release/*/app.dll >/tmp/kestrel.log 2>&1 &
app=$!
curl -sS -o /dev/null --retry 60 --retry-all-errors --retry-delay 1 https://localhost:7443/ || {{ echo 'kestrel not reachable'; cat /tmp/kestrel.log; kill $app; exit 98; }}
{body}
rc=$?
kill $app 2>/dev/null
exit $rc"
    )
}

const KESTREL_CHECKS: &str = "echo \"curl: $(curl -sS https://localhost:7443/)\"
echo \"curl-system: $(env -u CURL_CA_BUNDLE -u SSL_CERT_FILE -u SSL_CERT_DIR curl -sS https://localhost:7443/)\"
echo \"served: $(echo | openssl s_client -connect localhost:7443 -servername localhost 2>/dev/null | openssl x509 -noout -fingerprint -sha256)\"
echo \"imported: $(openssl x509 -in /etc/puddle/dev-cert/localhost.crt -noout -fingerprint -sha256)\"
dotnet dev-certs https --check --trust >/tmp/check.log 2>&1; echo \"check: $?\"";

fn fingerprints(out: &str) -> (String, String) {
    let find = |key: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap_or_else(|| panic!("no {key} line in:\n{out}"))
            .to_owned()
    };
    (find("served: "), find("imported: "))
}

async fn dotnet_run(env: &VmEnv) -> Result<(), HarnessError> {
    let builder = env
        .sandbox("devcert-dotnet", DOTNET_IMAGE)?
        .cpus(2)
        .memory(3072u32);
    env.scope(Box::pin(async {
        let sandbox = within("create", CREATE_BUDGET, builder.create()).await??;
        let (code, out) = sh(
            &sandbox,
            "tools",
            EXEC_BUDGET,
            "command -v curl openssl dotnet".to_owned(),
        )
        .await?;
        assert_eq!(code, 0, "the .NET image lacks a tool the test needs: {out}");

        let (code, out) = sh(
            &sandbox,
            "app",
            BUILD_BUDGET,
            "export HOME=/root DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1
dotnet new web -o /tmp/app >/dev/null && cd /tmp/app && dotnet build -c Release -nologo 2>&1 | tail -3"
                .to_owned(),
        )
        .await?;
        assert_eq!(code, 0, "building the web template failed: {out}");

        // Before the boot files .NET has no development certificate.
        let (_, out) = sh(
            &sandbox,
            "check before",
            EXEC_BUDGET,
            "HOME=/root dotnet dev-certs https --check --trust >/dev/null 2>&1; echo \"check: $?\"".to_owned(),
        )
        .await?;
        assert!(!out.lines().any(|l| l == "check: 0"), "{out}");

        let first = trust();
        let steps = apply(&sandbox, &first).await?;
        assert!(
            steps.contains("certificate imported into .NET and trusted for its check"),
            "{steps}"
        );
        let (code, out) = sh(&sandbox, "kestrel", BUILD_BUDGET, with_kestrel(KESTREL_CHECKS)).await?;
        eprintln!("kestrel with the imported certificate:\n{out}");
        assert_eq!(code, 0, "{out}");
        for line in ["curl: Hello World!", "curl-system: Hello World!", "check: 0"] {
            assert!(out.lines().any(|l| l == line), "missing {line:?} in:\n{out}");
        }
        let (served, imported) = fingerprints(&out);
        assert_eq!(served, imported, "Kestrel serves another certificate than the one imported");

        // A second boot with the same certificate changes nothing; a renewed one is imported.
        let steps = apply(&sandbox, &first).await?;
        assert!(steps.contains("certificate unchanged; .NET already has it"), "{steps}");
        let renewed = trust();
        let steps = apply(&sandbox, &renewed).await?;
        assert!(steps.contains("certificate imported into .NET"), "{steps}");
        let (code, out) = sh(&sandbox, "kestrel renewed", BUILD_BUDGET, with_kestrel(KESTREL_CHECKS)).await?;
        assert_eq!(code, 0, "{out}");
        assert!(out.lines().any(|l| l == "check: 0"), "{out}");
        let (served_now, imported_now) = fingerprints(&out);
        assert_eq!(served_now, imported_now);
        assert_ne!(served_now, served, "the renewed certificate was not served");

        within("stop", STOP_BUDGET, sandbox.stop()).await??;
        within("remove", STOP_BUDGET, Sandbox::remove(sandbox.name())).await??;
        Ok(())
    }))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_dev_cert_curl_node_and_the_system_store_trust_it_and_a_renewal_replaces_it() {
    let env = VmEnv::from_env().await.unwrap();
    let result = Box::pin(node_run(&env)).await;
    let cleaned = Box::pin(env.cleanup()).await;
    result.unwrap();
    cleaned.unwrap();
    env.remove_home().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_dev_cert_dotnet_and_kestrel_accept_the_ecdsa_certificate() {
    let env = VmEnv::from_env().await.unwrap();
    let result = Box::pin(dotnet_run(&env)).await;
    let cleaned = Box::pin(env.cleanup()).await;
    result.unwrap();
    cleaned.unwrap();
    env.remove_home().await.unwrap();
}
