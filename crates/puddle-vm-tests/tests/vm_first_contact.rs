// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which common tools accept a certificate the decrypting proxy's CA signed, on first contact,
//! on a real microVM (the Linux host; WHP on Windows).
//!
//! The guest gets Puddle's CA through the real `GuestTrust` files, environment and steps (as
//! `vm_ca_trust.rs` applies them). A fixture HTTPS server (`openssl s_server`), whose leaf the
//! proxy's CA signed and whose only name is `localhost`, stands where the proxy stands. Each tool
//! then contacts it in three conditions:
//!
//! - `bare`: before the trust is applied (the negative control: a tool that passes here trusts
//!   anything, and the row says nothing);
//! - `env`: after the trust is applied, in a shell that read the guest environment;
//! - `system`: after the trust is applied, with an empty environment (a process the guest
//!   environment did not reach: only the system store and the tool's own defaults).
//!
//! A tool is judged only by whether it refused the certificate (`tls-fail`) or went on to talk
//! HTTP (`tls-ok`, whatever the HTTP status): the fixture serves one page, so a registry that
//! answers 404 still shows that the handshake passed. The matrix goes to the log as
//! `matrix|<image>|<tool>|<condition>|<verdict>|<first output line>`; the list that seeds the
//! curated pass-through list is read from it.

#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "test code outside #[test] fns: a failed check fails the test, output goes to the log"
)]

use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use microsandbox::Sandbox;
use puddle_ca::{CaBuilder, TrustBundle};
use puddle_certs::{
    CorporateRoots, GuestTrust, Location, Physical, StoreName, StoreSnapshot, StoreSource,
};
use puddle_vm_tests::{HarnessError, VmEnv, create_from_registry, within};

const CREATE_BUDGET: Duration = Duration::from_mins(15);
const EXEC_BUDGET: Duration = Duration::from_mins(5);
const STOP_BUDGET: Duration = Duration::from_mins(1);

/// Text that marks a refused certificate in the output of the tools below.
const REFUSED: &str = "certificate verify failed|CERTIFICATE_VERIFY_FAILED|SSL certificate problem|UnknownIssuer|unable to verify|UNABLE_TO_VERIFY|x509|self.signed|verification failed|PKIX|unable to get local issuer|public key does not match|certificate problem|not trusted|SSL_ERROR|ssl3_get_server_certificate|tlsv1 alert|unknown ca|CERT_";

const GO_FETCH: &str = r#"package main

import (
	"fmt"
	"net/http"
	"os"
)

func main() {
	r, err := http.Get("https://localhost:8443/")
	if err != nil {
		fmt.Println(err)
		os.Exit(1)
	}
	fmt.Println("go: status", r.StatusCode)
}
"#;

/// A tool: a name and the shell line that contacts `https://localhost:8443/`.
type Tool = (&'static str, &'static str);

const COMMON: &[Tool] = &[
    ("curl", "curl -sS https://localhost:8443/ -o /dev/null"),
    ("wget", "wget -O /dev/null https://localhost:8443/"),
    ("git", "git ls-remote https://localhost:8443/x.git"),
    (
        "apt",
        "mkdir -p /tmp/aptlists/partial && printf 'deb https://localhost:8443/ stable main\\n' >/tmp/s.list && apt-get -o Dir::Etc::sourcelist=/tmp/s.list -o Dir::Etc::sourceparts=/nonexistent -o Dir::State::lists=/tmp/aptlists update",
    ),
    (
        "curl-pinned",
        "curl -sS --pinnedpubkey sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= https://localhost:8443/ -o /dev/null",
    ),
];

const PYTHON: &[Tool] = &[
    (
        "python-urllib",
        "python3 -c \"import urllib.request; urllib.request.urlopen('https://localhost:8443/')\"",
    ),
    (
        "python-requests-bundled-roots",
        "python3 -c \"from pip._vendor import requests; requests.get('https://localhost:8443/')\"",
    ),
    (
        "pip",
        "pip download --no-cache-dir --disable-pip-version-check --no-deps -d /tmp/dl --index-url https://localhost:8443/simple/ nothing",
    ),
];

const NODE: &[Tool] = &[
    (
        "node-fetch",
        "node -e \"fetch('https://localhost:8443/').then(r=>console.log('node: status',r.status)).catch(e=>{console.log(String(e), String(e.cause));process.exit(1)})\"",
    ),
    (
        "npm",
        "HOME=/tmp/npmhome npm view nothing --registry https://localhost:8443/ --fetch-retries=0",
    ),
];

const GO: &[Tool] = &[(
    "go",
    "cd /tmp && HOME=/root GOCACHE=/tmp/gocache go run /tmp/m.go",
)];

/// The (tool, condition) pairs that must be refused after the trust is applied; every other
/// pair must reach HTTP. Measured on the real images: a pinned client refuses any proxy
/// certificate, and Node `fetch` and Python `requests` with its bundled roots read only the
/// guest environment, not the system store.
const REFUSES_AFTER_TRUST: &[(&str, &str)] = &[
    ("curl-pinned", "env"),
    ("curl-pinned", "system"),
    ("node-fetch", "system"),
    ("python-requests-bundled-roots", "system"),
];

/// Compare the rows the probes printed against the pinned expectation, tool by tool.
fn check_after_trust(label: &str, tools: &[&Tool], out: &str) -> Result<(), String> {
    let mut wrong = Vec::new();
    for (name, _) in tools {
        for condition in ["env", "system"] {
            let prefix = format!("matrix|{label}|{name}|{condition}|");
            let rows: Vec<&str> = out.lines().filter(|l| l.starts_with(&prefix)).collect();
            let [row] = rows.as_slice() else {
                wrong.push(format!("{name}/{condition}: {} rows, want 1", rows.len()));
                continue;
            };
            let verdict = if REFUSES_AFTER_TRUST.contains(&(name, condition)) {
                "tls-fail"
            } else {
                "tls-ok"
            };
            if !row[prefix.len()..].starts_with(verdict) {
                wrong.push(format!("{name}/{condition}: want {verdict}, got {row}"));
            } else if verdict == "tls-ok" && row.contains("not found") {
                // A missing binary prints no certificate error, so it would read as tls-ok.
                wrong.push(format!("{name}/{condition}: the tool did not run: {row}"));
            }
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        Err(wrong.join("\n"))
    }
}

struct Fixture {
    trust: GuestTrust,
    leaf_pem: String,
    leaf_key: String,
}

/// Puddle's CA in the trust bundle, as a sandbox has it, and a leaf it signed.
fn fixture() -> Fixture {
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Puddle proxy CA (VM test)");
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let ca_cert = ca.self_signed(&ca_key).unwrap();

    let mut leaf = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    leaf.is_ca = rcgen::IsCa::ExplicitNoCa;
    leaf.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    leaf.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::from_params(&ca, &ca_key);
    let leaf_cert = leaf.signed_by(&leaf_key, &issuer).unwrap();

    let mut snapshot = StoreSnapshot::new();
    snapshot.add(
        StoreSource::new(Location::CurrentUser, StoreName::Root, Physical::Default),
        ca_cert.der().to_vec(),
    );
    let corporate = CorporateRoots::select(&snapshot, SystemTime::now());
    let other = CaBuilder::new("Puddle proxy CA (VM test, other)")
        .build()
        .unwrap();
    let bundle = TrustBundle::new().with(other.certificate().clone());
    Fixture {
        trust: GuestTrust::new(&corporate, &bundle),
        leaf_pem: leaf_cert.pem(),
        leaf_key: leaf_key.serialize_pem(),
    }
}

/// The uploads and the script that does what `boot.sh` does with this trust.
fn install(trust: &GuestTrust) -> (Vec<(String, Vec<u8>)>, String) {
    let mut uploads = Vec::new();
    let mut script = String::from("set -e\n");
    for (n, f) in trust.guest_files().iter().enumerate() {
        let tmp = format!("/tmp/catrust-{n}");
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
    uploads.push(("/tmp/catrust-env".to_owned(), env.into_bytes()));
    script.push_str(
        "mkdir -p /etc/profile.d && cp /tmp/catrust-env /etc/profile.d/01-puddle-env.sh\n",
    );
    script.push_str("if command -v update-ca-certificates >/dev/null 2>&1; then update-ca-certificates >/dev/null; fi\n");
    for step in trust.boot_steps() {
        writeln!(script, "sh {step} >/dev/null").unwrap();
    }
    (uploads, script)
}

async fn sh(sandbox: &Sandbox, what: &'static str, script: String) -> Result<String, HarnessError> {
    let out = within(what, EXEC_BUDGET, sandbox.shell(script)).await??;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(out.stdout_bytes()),
        String::from_utf8_lossy(out.stderr_bytes())
    ))
}

/// One tool in one condition, as the guest runs it, and the matrix line it makes.
fn probe(image: &str, tool: &Tool, condition: &str, prefix: &str) -> String {
    let (name, cmd) = tool;
    let wrapped = if condition == "system" {
        format!(
            "env -i HOME=/root PATH=\"$PATH\" sh -c '{}'",
            cmd.replace('\'', "'\\''")
        )
    } else {
        format!("{prefix}{cmd}")
    };
    format!(
        "out=$(timeout 90 sh -c {q} 2>&1 </dev/null); rc=$?
if printf '%s' \"$out\" | grep -qiE '{REFUSED}'; then v=tls-fail; else v=tls-ok; fi
echo \"matrix|{image}|{name}|{condition}|$v|rc=$rc|$(printf '%s' \"$out\" | head -n 2 | tr '\\n|' '  ' | cut -c1-160)\"
",
        q = shell_quote(&wrapped)
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

async fn matrix_run(
    env: &VmEnv,
    image: &'static str,
    label: &'static str,
    tools: &[Tool],
    extra_upload: Option<(&str, &str)>,
) -> Result<(), HarnessError> {
    let f = fixture();
    let builder = env.sandbox(label, image)?.memory(1024u32);
    env.scope(Box::pin(async {
        let sandbox = create_from_registry(image, CREATE_BUDGET, builder.create()).await?;
        let fs = sandbox.fs();
        let mut files = vec![
            ("/tmp/leaf.pem".to_owned(), f.leaf_pem.clone().into_bytes()),
            ("/tmp/leaf.key".to_owned(), f.leaf_key.clone().into_bytes()),
        ];
        if let Some((path, text)) = extra_upload {
            files.push((path.to_owned(), text.as_bytes().to_vec()));
        }
        let (uploads, apply) = install(&f.trust);
        files.extend(uploads);
        for (path, data) in files {
            within("write", EXEC_BUDGET, fs.write(&path, data)).await??;
        }
        let start = sh(
            &sandbox,
            "server",
            "(openssl s_server -www -accept 8443 -cert /tmp/leaf.pem -key /tmp/leaf.key >/tmp/server.log 2>&1 &) ; sleep 2; cat /tmp/server.log".to_owned(),
        )
        .await?;
        eprintln!("server: {start}");

        let mut all = String::new();
        let list: Vec<&Tool> = COMMON.iter().chain(tools).collect();
        for tool in &list {
            all.push_str(&probe(label, tool, "bare", ""));
        }
        let out = sh(&sandbox, "bare", all).await?;
        eprintln!("{out}");
        let bare_pass: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("matrix|") && l.contains("|tls-ok|"))
            .collect();
        eprintln!("negative control, tools that trusted the CA before the trust: {bare_pass:?}");

        let applied = sh(&sandbox, "apply", format!("{apply}\necho applied")).await?;
        eprintln!("apply: {applied}");
        assert!(applied.contains("applied"), "{applied}");

        let mut all = String::new();
        for tool in &list {
            all.push_str(&probe(label, tool, "env", ". /etc/profile.d/01-puddle-env.sh; "));
            all.push_str(&probe(label, tool, "system", ""));
        }
        let after = sh(&sandbox, "after", all).await?;
        eprintln!("{after}");

        within("stop", STOP_BUDGET, sandbox.stop()).await??;
        within("remove", STOP_BUDGET, Sandbox::remove(sandbox.name())).await??;
        // The control: curl must refuse the certificate before the trust is applied.
        assert!(
            !bare_pass.iter().any(|l| l.contains("|curl|bare|")),
            "curl trusted the fixture CA before the trust: the rows say nothing"
        );
        let verdicts = check_after_trust(label, &list, &after);
        assert!(
            verdicts.is_ok(),
            "tools after the trust differ from the pinned list:\n{}",
            verdicts.unwrap_err()
        );
        Ok(())
    }))
    .await
}

async fn in_env<F>(run: F)
where
    F: for<'a> FnOnce(
        &'a VmEnv,
    )
        -> std::pin::Pin<Box<dyn Future<Output = Result<(), HarnessError>> + 'a>>,
{
    let env = VmEnv::from_env().await.unwrap();
    let result = run(&env).await;
    let cleaned = Box::pin(env.cleanup()).await;
    result.unwrap();
    cleaned.unwrap();
    env.remove_home().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_first_contact_python_image_tools_accept_the_proxy_ca() {
    in_env(|env| Box::pin(matrix_run(env, "python:3.12", "fc-python", PYTHON, None))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_first_contact_node_image_tools_accept_the_proxy_ca() {
    in_env(|env| Box::pin(matrix_run(env, "node:22", "fc-node", NODE, None))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_first_contact_go_image_tools_accept_the_proxy_ca() {
    in_env(|env| {
        Box::pin(matrix_run(
            env,
            "golang:1.23",
            "fc-go",
            GO,
            Some(("/tmp/m.go", GO_FETCH)),
        ))
    })
    .await;
}
