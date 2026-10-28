// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest CA trust for tools with their own bundles, on a real microVM (tier K on Linux KVM,
//! W on Windows WHP).
//!
//! The guest gets Puddle's CA through the plan's own files, `update-ca-certificates` and boot
//! steps, as in `vm_root_sync.rs`. A fixture HTTPS server, whose certificate a CA issues the way
//! the decrypting proxy's CA issues a leaf (an unconstrained CA, an ECDSA leaf with the host as
//! its only name, server-auth only), stands where the proxy stands; one image per tool:
//!
//! - `eclipse-temurin:21-jdk`: a Java program fetches `https://localhost:8443/` with its default
//!   trust. Only the `cacerts` import of the Java boot step can make that work (Java reads no
//!   variable).
//! - `denoland/deno:debian`: `fetch` works through `DENO_CERT` (Deno keeps its own roots).
//! - `python:3.13-slim`: `pip download` from an index at that URL works through `PIP_CERT`,
//!   and reads the variable (an empty bundle in it fails the same fetch).
//!
//! Each tool fails before the files are applied (the negative control), so a pass is the trust
//! and not a trusting image. The wire peer is the fixture server, not the proxy process: the
//! proxy side is covered by the `puddle-proxy` tests.
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
use puddle_vm_tests::{HarnessError, VmEnv, within};

const JAVA_IMAGE: &str = "eclipse-temurin:21-jdk";
const DENO_IMAGE: &str = "denoland/deno:debian";
const PIP_IMAGE: &str = "python:3.13-slim";
const CREATE_BUDGET: Duration = Duration::from_mins(15);
const EXEC_BUDGET: Duration = Duration::from_mins(3);
const STOP_BUDGET: Duration = Duration::from_mins(1);

const FETCH_JAVA: &str = r#"
import java.io.*;
import java.net.*;
import java.nio.file.*;
import java.security.*;
import java.security.cert.*;
import java.security.spec.*;
import java.util.*;
import javax.net.ssl.*;

public class Fetch {
    public static void main(String[] args) throws Exception {
        X509Certificate leaf;
        try (InputStream in = Files.newInputStream(Path.of("/tmp/leaf.pem"))) {
            leaf = (X509Certificate) CertificateFactory.getInstance("X.509").generateCertificate(in);
        }
        String pem = Files.readString(Path.of("/tmp/leaf.key"))
            .replaceAll("-----[A-Z ]+-----", "").replaceAll("\\s", "");
        PrivateKey key = KeyFactory.getInstance("EC")
            .generatePrivate(new PKCS8EncodedKeySpec(Base64.getDecoder().decode(pem)));
        KeyStore ks = KeyStore.getInstance("PKCS12");
        ks.load(null, null);
        ks.setKeyEntry("server", key, new char[0], new java.security.cert.Certificate[] {leaf});
        KeyManagerFactory kmf = KeyManagerFactory.getInstance("SunX509");
        kmf.init(ks, new char[0]);
        SSLContext ctx = SSLContext.getInstance("TLS");
        ctx.init(kmf.getKeyManagers(), null, null);
        SSLServerSocket server = (SSLServerSocket) ctx.getServerSocketFactory()
            .createServerSocket(8443, 5, InetAddress.getByName("127.0.0.1"));
        Thread t = new Thread(() -> {
            while (true) {
                try (Socket s = server.accept()) {
                    s.getInputStream().read(new byte[4096]);
                    s.getOutputStream().write(
                        "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\npuddle-ok".getBytes());
                } catch (IOException e) {
                    // A failed handshake is the client refusing the certificate.
                }
            }
        });
        t.setDaemon(true);
        t.start();
        HttpsURLConnection c = (HttpsURLConnection) new URL("https://localhost:8443/ok.txt").openConnection();
        System.out.println("java: " + new String(c.getInputStream().readAllBytes()));
    }
}
"#;

const FETCH_DENO: &str = r#"
const ac = new AbortController();
Deno.serve(
  {
    hostname: "127.0.0.1",
    port: 8443,
    cert: Deno.readTextFileSync("/tmp/leaf.pem"),
    key: Deno.readTextFileSync("/tmp/leaf.key"),
    signal: ac.signal,
    onListen() {},
  },
  () => new Response("puddle-ok"),
);
try {
  const r = await fetch("https://localhost:8443/ok.txt");
  console.log("deno: " + (await r.text()));
} finally {
  ac.abort();
}
"#;

/// An index with one wheel (built here, so the image needs nothing but Python) behind TLS.
const PIP_SERVER_PY: &str = r#"
import http.server, ssl, os, zipfile
os.makedirs('/tmp/www/simple/puddletest', exist_ok=True)
whl = 'puddletest-1.0-py3-none-any.whl'
with zipfile.ZipFile('/tmp/www/simple/puddletest/' + whl, 'w') as z:
    z.writestr('puddletest/__init__.py', 'VALUE = 1\n')
    z.writestr('puddletest-1.0.dist-info/METADATA', 'Metadata-Version: 2.1\nName: puddletest\nVersion: 1.0\n')
    z.writestr('puddletest-1.0.dist-info/WHEEL', 'Wheel-Version: 1.0\nGenerator: test\nRoot-Is-Purelib: true\nTag: py3-none-any\n')
    z.writestr('puddletest-1.0.dist-info/RECORD', '')
with open('/tmp/www/simple/puddletest/index.html', 'w') as f:
    f.write('<html><body><a href="' + whl + '">' + whl + '</a></body></html>')
os.chdir('/tmp/www')
srv = http.server.HTTPServer(('127.0.0.1', 8443), http.server.SimpleHTTPRequestHandler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain('/tmp/leaf.pem', '/tmp/leaf.key')
srv.socket = ctx.wrap_socket(srv.socket, server_side=True)
srv.serve_forever()
"#;

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

    // The CA reaches the guest as a user-added root in the host snapshot would, so the extra CAs
    // file holds it; a second, unrelated Puddle CA sits in the bundle next to it.
    let mut snapshot = StoreSnapshot::new();
    snapshot.add(
        StoreSource::new(Location::CurrentUser, StoreName::Root, Physical::Default),
        ca_cert.der().to_vec(),
    );
    let corporate = CorporateRoots::select(&snapshot, SystemTime::now());
    assert_eq!(corporate.certificates().len(), 1);
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

/// The uploads (`/tmp/catrust-<n>`, the env file) and the script that does what `boot.sh` does
/// with this trust: each file with its mode, the env file, `update-ca-certificates` where the
/// image has it, the plan's steps in order.
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
        writeln!(script, "echo \"step {step}: $(sh {step})\"").unwrap();
    }
    (uploads, script)
}

async fn sh(
    sandbox: &Sandbox,
    what: &'static str,
    script: String,
) -> Result<(i32, String), HarnessError> {
    let out = within(what, EXEC_BUDGET, sandbox.shell(script)).await??;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(out.stdout_bytes()),
        String::from_utf8_lossy(out.stderr_bytes())
    );
    Ok((out.status().code, text))
}

/// Uploads the fixture files and the client program, applies the trust; returns the steps' text.
async fn prepare(
    sandbox: &Sandbox,
    f: &Fixture,
    program: (&str, &str),
) -> Result<String, HarnessError> {
    let fs = sandbox.fs();
    for (path, data) in [
        (program.0, program.1.as_bytes().to_vec()),
        ("/tmp/leaf.pem", f.leaf_pem.clone().into_bytes()),
        ("/tmp/leaf.key", f.leaf_key.clone().into_bytes()),
    ] {
        within("write", EXEC_BUDGET, fs.write(path, data)).await??;
    }
    let (uploads, script) = install(&f.trust);
    for (path, data) in uploads {
        within("write", EXEC_BUDGET, fs.write(&path, data)).await??;
    }
    Ok(script)
}

async fn apply(sandbox: &Sandbox, script: String) -> Result<String, HarnessError> {
    let (code, out) = sh(sandbox, "apply", script).await?;
    eprintln!("applied:\n{out}");
    assert_eq!(code, 0, "apply failed: {out}");
    Ok(out)
}

async fn finish(sandbox: &Sandbox) -> Result<(), HarnessError> {
    within("stop", STOP_BUDGET, sandbox.stop()).await??;
    within("remove", STOP_BUDGET, Sandbox::remove(sandbox.name())).await??;
    Ok(())
}

async fn java_run(env: &VmEnv) -> Result<(), HarnessError> {
    let f = fixture();
    let builder = env.sandbox("catrust-java", JAVA_IMAGE)?.memory(1024u32);
    env.scope(Box::pin(async {
        let sandbox = within("create", CREATE_BUDGET, builder.create()).await??;
        let script = prepare(&sandbox, &f, ("/tmp/Fetch.java", FETCH_JAVA)).await?;
        let run = "cd /tmp && java Fetch.java 2>&1";
        let (code, out) = sh(&sandbox, "java before", run.to_owned()).await?;
        assert_ne!(code, 0, "Java trusted the CA before the boot files: {out}");
        assert!(out.contains("PKIX") || out.contains("certificate"), "{out}");

        let steps = apply(&sandbox, script.clone()).await?;
        assert!(
            steps.contains("java-trust: ") && steps.contains(" added, 0 removed"),
            "{steps}"
        );
        let (code, out) = sh(&sandbox, "java after", run.to_owned()).await?;
        assert_eq!(
            code, 0,
            "Java refused the certificate after the import: {out}"
        );
        assert!(out.lines().any(|l| l == "java: puddle-ok"), "{out}");

        // A second boot finds the store as it left it.
        let steps = apply(&sandbox, script).await?;
        assert!(steps.contains("unchanged"), "{steps}");
        finish(&sandbox).await
    }))
    .await
}

async fn deno_run(env: &VmEnv) -> Result<(), HarnessError> {
    let f = fixture();
    let builder = env.sandbox("catrust-deno", DENO_IMAGE)?;
    env.scope(Box::pin(async {
        let sandbox = within("create", CREATE_BUDGET, builder.create()).await??;
        let script = prepare(&sandbox, &f, ("/tmp/fetch.ts", FETCH_DENO)).await?;
        let run = "export HOME=/root; deno run -A /tmp/fetch.ts 2>&1";
        let (code, out) = sh(&sandbox, "deno before", run.to_owned()).await?;
        assert_ne!(code, 0, "Deno trusted the CA before the boot files: {out}");

        apply(&sandbox, script).await?;
        let with_env = format!(". /etc/profile.d/01-puddle-env.sh; {run}");
        let (code, out) = sh(&sandbox, "deno after", with_env).await?;
        assert_eq!(
            code, 0,
            "Deno refused the certificate with DENO_CERT: {out}"
        );
        assert!(out.lines().any(|l| l == "deno: puddle-ok"), "{out}");

        // DENO_CERT is what does it: the other variables are set and Deno ignores them.
        let without = format!(". /etc/profile.d/01-puddle-env.sh; unset DENO_CERT; {run}");
        let (code, out) = sh(&sandbox, "deno without DENO_CERT", without).await?;
        assert_ne!(code, 0, "Deno trusted the CA without DENO_CERT: {out}");
        finish(&sandbox).await
    }))
    .await
}

async fn pip_run(env: &VmEnv) -> Result<(), HarnessError> {
    let f = fixture();
    let builder = env.sandbox("catrust-pip", PIP_IMAGE)?;
    env.scope(Box::pin(async {
        let sandbox = within("create", CREATE_BUDGET, builder.create()).await??;
        let script = prepare(&sandbox, &f, ("/tmp/server.py", PIP_SERVER_PY)).await?;
        let serve = |pre: &str| {
            format!(
                "set -u
{pre}
python3 /tmp/server.py >/tmp/server.log 2>&1 &
srv=$!
i=0
until [ -f /tmp/www/simple/puddletest/index.html ] && python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", 8443), 1)' 2>/dev/null; do
  i=$((i+1)); [ $i -gt 100 ] && {{ echo 'server did not start'; cat /tmp/server.log; kill $srv; exit 99; }}
  sleep 0.1
done
rm -rf /tmp/dl
pip download --no-cache-dir --disable-pip-version-check --no-deps -d /tmp/dl --index-url https://localhost:8443/simple/ puddletest 2>&1
rc=$?
kill $srv 2>/dev/null
ls /tmp/dl 2>/dev/null
exit $rc"
            )
        };
        let (code, out) = sh(&sandbox, "pip before", serve("true")).await?;
        assert_ne!(code, 0, "pip trusted the CA before the boot files: {out}");
        assert!(!out.contains("puddletest-1.0-py3-none-any.whl\nSuccessfully"), "{out}");

        apply(&sandbox, script).await?;
        let (code, out) = sh(
            &sandbox,
            "pip after",
            serve(". /etc/profile.d/01-puddle-env.sh"),
        )
        .await?;
        assert_eq!(code, 0, "pip refused the certificate with PIP_CERT: {out}");
        assert!(out.contains("Successfully downloaded puddletest"), "{out}");

        // pip reads PIP_CERT (the variable, not only the system store the image's own
        // `update-ca-certificates` filled): an empty bundle in it makes the same fetch fail.
        let (code, out) = sh(
            &sandbox,
            "pip with an empty PIP_CERT",
            serve(". /etc/profile.d/01-puddle-env.sh; : >/tmp/empty.pem; export PIP_CERT=/tmp/empty.pem"),
        )
        .await?;
        assert_ne!(code, 0, "pip ignored PIP_CERT: {out}");
        finish(&sandbox).await
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
async fn vm_ca_trust_java_trusts_the_ca_through_its_cacerts() {
    in_env(|env| Box::pin(java_run(env))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_ca_trust_deno_trusts_the_ca_through_deno_cert() {
    in_env(|env| Box::pin(deno_run(env))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_ca_trust_pip_trusts_the_ca_through_pip_cert() {
    in_env(|env| Box::pin(pip_run(env))).await;
}
