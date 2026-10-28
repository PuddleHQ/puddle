// SPDX-License-Identifier: GPL-3.0-or-later
//! Root sync through the real `boot.sh` and `guest/ca-bundle.sh`, on the test host against a
//! fake guest root (stubbed `sysctl` and a Debian-like `update-ca-certificates`), under dash and
//! bash. The VM version is `puddle-vm-tests/tests/vm_root_sync.rs`.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns: a failed setup fails the test"
)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use puddle_boot::{BOOT_SH, BootPlan, ENV_FILE_GUEST};
use puddle_ca::{CaBuilder, CaCertificate, TrustBundle};
use puddle_certs::devcert::{DevCertManager, GuestDevCert, Unsupported};
use puddle_certs::{
    BUNDLE_STEP_SH, CA_BUNDLE_PATH, CorporateRoots, EXTRA_CAS_PATH, GuestTrust, HOST_CA_DIR,
    SOURCES, StoreSnapshot,
};
use puddle_compute::ImageConfig;

/// The agent binary as the merge tool: reads the spec and leaves the file as it is.
const MERGE_TOOL_STUB: &str = "#!/bin/sh\ncat >/dev/null\necho unchanged\n";

const SYSCTL_STUB: &str = r#"#!/bin/sh
[ "$1" = -w ] && shift
key=${1%%=*}
printf '%s\n' "${1#*=}" >"$PUDDLE_ROOT/proc/sys/$(printf %s "$key" | tr . /)"
"#;

/// Like Debian's: the distro's roots plus every `*.crt` below `/usr/local/share/ca-certificates`
/// (subdirectories too), concatenated into the system bundle.
const UPDATE_CA_STUB: &str = r#"#!/bin/sh
echo "update-ca-certificates" >>"$PUDDLE_ROOT/calls"
mkdir -p "$PUDDLE_ROOT/etc/ssl/certs"
{
    cat "$PUDDLE_ROOT/usr/share/ca-certificates/distro.pem"
    find "$PUDDLE_ROOT/usr/local/share/ca-certificates" -name '*.crt' -type f | sort | while read -r f; do cat "$f"; done
} >"$PUDDLE_ROOT/etc/ssl/certs/ca-certificates.crt"
"#;

struct Guest {
    root: PathBuf,
    shell: PathBuf,
    path: String,
}

fn shells() -> Vec<PathBuf> {
    let found: Vec<PathBuf> = ["/bin/dash", "/bin/bash"]
        .iter()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .collect();
    assert!(!found.is_empty(), "no POSIX shell found");
    found
}

fn write_exec(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Guest {
    /// A fake Debian-like guest whose distro bundle holds `distro`; `update_ca` says whether the
    /// image has `update-ca-certificates`.
    fn new(shell: &Path, distro: &str, update_ca: bool) -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let root = std::env::temp_dir().join(format!(
            "puddle-certs-test-{}-{}-{nanos}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        for d in ["proc/sys/fs/inotify", "proc/sys/kernel/random", "etc"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        for f in [
            "fs/inotify/max_user_instances",
            "fs/inotify/max_user_watches",
            "kernel/unprivileged_bpf_disabled",
        ] {
            std::fs::write(root.join("proc/sys").join(f), "0\n").unwrap();
        }
        std::fs::write(root.join("proc/sys/kernel/random/boot_id"), "boot-1\n").unwrap();
        write_exec(&root.join("puddle/boot.sh"), BOOT_SH);
        // Every plan merges VS Code's Machine settings through the agent binary.
        write_exec(&root.join("puddle/puddle-agent"), MERGE_TOOL_STUB);
        let stubs = root.join("stubs");
        write_exec(&stubs.join("sysctl"), SYSCTL_STUB);
        let g = Self {
            path: format!("{}:/usr/bin:/bin", stubs.display()),
            root,
            shell: shell.to_owned(),
        };
        if update_ca {
            write_exec(&stubs.join("update-ca-certificates"), UPDATE_CA_STUB);
            g.write("/usr/share/ca-certificates/distro.pem", distro);
            // The image as built: its bundle already made.
            g.write("/etc/ssl/certs/ca-certificates.crt", distro);
        } else if !distro.is_empty() {
            g.write("/etc/pki/tls/certs/ca-bundle.crt", distro);
        }
        g
    }

    fn p(&self, guest: &str) -> PathBuf {
        self.root.join(guest.trim_start_matches('/'))
    }

    fn write(&self, guest: &str, text: &str) {
        let p = self.p(guest);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn read(&self, guest: &str) -> String {
        std::fs::read_to_string(self.p(guest)).unwrap_or_else(|e| panic!("{guest}: {e}"))
    }

    fn calls(&self, what: &str) -> usize {
        std::fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == what)
            .count()
    }

    fn boot(&self, trust: &GuestTrust) -> Output {
        let mut b = BootPlan::builder(&ImageConfig::default())
            .no_agent()
            .files(trust.guest_files())
            .env(&trust.env());
        for step in trust.boot_steps() {
            b = b.step(step);
        }
        let plan = b.build().unwrap();
        let mut child = Command::new(&self.shell)
            .arg(self.p("/puddle/boot.sh"))
            .env_clear()
            .env("PATH", &self.path)
            .env("PUDDLE_ROOT", &self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&plan.render())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// Runs only the step script with `path` as `PATH`.
    fn step(&self, path: &str) -> Output {
        let script = self.root.join("ca-bundle.sh");
        write_exec(&script, BUNDLE_STEP_SH);
        Command::new(&self.shell)
            .arg(script)
            .env_clear()
            .env("PATH", path)
            .env("PUDDLE_ROOT", &self.root)
            .output()
            .unwrap()
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn show(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn root_pem(cn: &str) -> (Vec<u8>, String) {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, cn);
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.not_after = rcgen::date_time_ymd(2040, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = p.self_signed(&key).unwrap();
    (cert.der().to_vec(), cert.pem().replace("\r\n", "\n"))
}

fn puddle_ca() -> CaCertificate {
    CaBuilder::new("Puddle proxy CA (test)")
        .build()
        .unwrap()
        .certificate()
        .clone()
}

fn trust(host: &[&Vec<u8>], puddle: &[&CaCertificate]) -> GuestTrust {
    let mut s = StoreSnapshot::new();
    for der in host {
        s.add(SOURCES[0], (*der).clone());
    }
    let now = UNIX_EPOCH + Duration::from_hours(500_000);
    let bundle = puddle
        .iter()
        .fold(TrustBundle::new(), |b, c| b.with((*c).clone()));
    GuestTrust::new(&CorporateRoots::select(&s, now), &bundle)
}

/// How often each certificate (by its base64 body) appears in `pem`, in first-seen order.
fn counts(pem: &str) -> Vec<(String, usize)> {
    let mut order = Vec::new();
    let mut n: BTreeMap<String, usize> = BTreeMap::new();
    for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
        let body: String = block
            .split("-----END CERTIFICATE-----")
            .next()
            .unwrap()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if !n.contains_key(&body) {
            order.push(body.clone());
        }
        *n.entry(body).or_default() += 1;
    }
    order
        .into_iter()
        .map(|b| {
            let c = n.get(&b).copied().unwrap_or_default();
            (b, c)
        })
        .collect()
}

fn body(pem: &str) -> String {
    counts(pem).remove(0).0
}

#[test]
fn first_boot_appends_roots_and_puddle_cas_to_the_distro_bundle_each_once() {
    let (_, d1) = root_pem("Distro Root 1");
    let (_, d2) = root_pem("Distro Root 2");
    // A corporate root that is also public: in the distro bundle and in the Windows store.
    let (public_der, public) = root_pem("Public Corp Root");
    let (corp_der, corp) = root_pem("Corp Root");
    let ca = puddle_ca();
    let t = trust(&[&corp_der, &public_der], &[&ca]);
    for sh in shells() {
        let g = Guest::new(&sh, &format!("{d1}{d2}{public}"), true);
        let out = g.boot(&t);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("update-ca-certificates ran"), "{stdout}");
        assert!(
            stdout
                .contains("bundle updated, 5 certificates with /etc/ssl/certs/ca-certificates.crt"),
            "{stdout}"
        );
        assert_eq!(g.calls("update-ca-certificates"), 1);

        let bundle = g.read(CA_BUNDLE_PATH);
        let c = counts(&bundle);
        assert!(c.iter().all(|(_, n)| *n == 1), "a certificate twice");
        let bodies: Vec<&str> = c.iter().map(|(b, _)| b.as_str()).collect();
        // Distro roots first, in their order, then the rest.
        assert_eq!(&bodies[..3], [body(&d1), body(&d2), body(&public)]);
        for extra in [corp.as_str(), ca.pem()] {
            assert!(bodies.contains(&body(extra).as_str()));
        }
        // The system bundle got them too (through update-ca-certificates).
        let system = g.read("/etc/ssl/certs/ca-certificates.crt");
        assert!(system.contains(&body(&corp)[..60]));
        // Node's extra file: host roots and Puddle's CA only.
        assert_eq!(counts(&g.read(EXTRA_CAS_PATH)).len(), 3);
        let env = g.read(ENV_FILE_GUEST);
        assert!(
            env.contains(&format!("export SSL_CERT_FILE='{CA_BUNDLE_PATH}'")),
            "{env}"
        );
        assert!(env.contains(&format!("export NODE_EXTRA_CA_CERTS='{EXTRA_CAS_PATH}'")));
        let mode = std::fs::metadata(g.p(CA_BUNDLE_PATH))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
    }
}

#[test]
fn unchanged_roots_skip_update_ca_certificates_and_leave_the_bundle_alone() {
    let (_, d1) = root_pem("Distro Root");
    let (corp_der, _) = root_pem("Corp Root");
    let t = trust(&[&corp_der], &[]);
    for sh in shells() {
        let g = Guest::new(&sh, &d1, true);
        assert!(g.boot(&t).status.success());
        let before = std::fs::metadata(g.p(CA_BUNDLE_PATH)).unwrap();
        let out = g.boot(&t);
        assert!(out.status.success(), "{}", show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("update-ca-certificates skipped"),
            "{stdout}"
        );
        assert!(stdout.contains("bundle unchanged"), "{stdout}");
        assert_eq!(g.calls("update-ca-certificates"), 1);
        let after = std::fs::metadata(g.p(CA_BUNDLE_PATH)).unwrap();
        assert_eq!(
            (before.ino(), before.mtime_nsec()),
            (after.ino(), after.mtime_nsec())
        );
    }
}

#[test]
fn a_root_removed_on_the_host_leaves_the_guest() {
    let (_, d1) = root_pem("Distro Root");
    let (a_der, a) = root_pem("Corp Root A");
    let (b_der, b) = root_pem("Corp Root B");
    let g = Guest::new(&shells()[0], &d1, true);
    assert!(g.boot(&trust(&[&a_der, &b_der], &[])).status.success());
    assert!(g.read(CA_BUNDLE_PATH).contains(&body(&b)[..60]));
    let out = g.boot(&trust(&[&a_der], &[]));
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(g.calls("update-ca-certificates"), 2);
    let bundle = g.read(CA_BUNDLE_PATH);
    assert!(bundle.contains(&body(&a)[..60]));
    assert!(!bundle.contains(&body(&b)[..60]));
    assert!(
        !g.read("/etc/ssl/certs/ca-certificates.crt")
            .contains(&body(&b)[..60])
    );
    let left = std::fs::read_dir(g.p(HOST_CA_DIR)).unwrap().count();
    assert_eq!(left, 1);
}

#[test]
fn an_image_without_update_ca_certificates_still_gets_distro_plus_extras() {
    let (_, d1) = root_pem("Fedora Root");
    let (corp_der, corp) = root_pem("Corp Root");
    // Fedora's bundle has comment lines and CRLF here; neither may break the merge.
    let distro = format!("# Fedora Root\n{}", d1.replace('\n', "\r\n"));
    let t = trust(&[&corp_der], &[]);
    for sh in shells() {
        let g = Guest::new(&sh, &distro, false);
        let out = g.boot(&t);
        assert!(out.status.success(), "{}", show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("update-ca-certificates skipped (not in this image)"),
            "{stdout}"
        );
        assert!(
            stdout.contains("with /etc/pki/tls/certs/ca-bundle.crt"),
            "{stdout}"
        );
        let bundle = g.read(CA_BUNDLE_PATH);
        assert!(!bundle.contains('\r') && !bundle.contains("# Fedora"));
        let bodies: Vec<String> = counts(&bundle).into_iter().map(|(b, _)| b).collect();
        assert_eq!(bodies, [body(&d1), body(&corp)]);
    }
}

#[test]
fn no_distro_bundle_gives_the_extras_alone() {
    let (corp_der, corp) = root_pem("Corp Root");
    let g = Guest::new(&shells()[0], "", false);
    let out = g.boot(&trust(&[&corp_der], &[]));
    assert!(out.status.success(), "{}", show(&out));
    assert!(String::from_utf8_lossy(&out.stdout).contains("(no distro bundle)"));
    assert_eq!(counts(&g.read(CA_BUNDLE_PATH)), [(body(&corp), 1)]);
}

#[test]
fn without_awk_the_bundle_is_complete_if_not_deduplicated() {
    let (_, d1) = root_pem("Distro Root");
    let (corp_der, _) = root_pem("Corp Root");
    let g = Guest::new(&shells()[0], &d1, true);
    assert!(g.boot(&trust(&[&corp_der], &[])).status.success());
    // A PATH with the script's tools but no awk.
    let bin = g.root.join("no-awk");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["cat", "chmod", "cmp", "mv", "grep", "rm"] {
        let found = ["/usr/bin", "/bin"]
            .iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.exists())
            .unwrap();
        std::os::unix::fs::symlink(found, bin.join(tool)).unwrap();
    }
    let out = g.step(&bin.display().to_string());
    assert!(out.status.success(), "{}", show(&out));
    let c = counts(&g.read(CA_BUNDLE_PATH));
    // The system bundle already holds the corporate root, so it shows up twice.
    assert_eq!(c.len(), 2);
    assert_eq!(c.iter().map(|(_, n)| n).sum::<usize>(), 3);
}

#[test]
fn the_step_fails_clearly_without_its_input_or_with_a_directory_in_the_way() {
    let g = Guest::new(&shells()[0], "", false);
    let out = g.step("/usr/bin:/bin");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("ca-bundle: {EXTRA_CAS_PATH} is missing\n")
    );
    g.write(EXTRA_CAS_PATH, "");
    std::fs::create_dir_all(g.p(CA_BUNDLE_PATH)).unwrap();
    let out = g.step("/usr/bin:/bin");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        format!("ca-bundle: {CA_BUNDLE_PATH} is a directory\n")
    );
}

// --- the development certificate (the plan's dev-cert step, through the real boot.sh) -------------

/// Stands in for the .NET SDK: logs what it was asked with the environment that matters, opens
/// the import file with the password it was given (so the file is a real PKCS#12 holding the
/// certificate), refuses `--check` while `$PUDDLE_ROOT/dotnet.refuse` exists and exits 4 on a bare `--trust` while `dotnet.trust-exit4` exists.
const DOTNET_STUB: &str = r#"#!/bin/sh
echo "$* | HOME=$HOME SSL_CERT_DIR=$SSL_CERT_DIR GEN=$DOTNET_GENERATE_ASPNET_CERTIFICATE" >>"$PUDDLE_ROOT/dotnet.log"
case "$*" in
*--import*)
    # dev-certs https --clean --import <file> -p <password>
    PW=$7 openssl pkcs12 -in "$5" -nokeys -password env:PW >"$PUDDLE_ROOT/dotnet.import-cert" 2>/dev/null || exit 3
    cp "$5" "$PUDDLE_ROOT/dotnet.pfx-seen" 2>/dev/null
    ;;
*--check*)
    if [ -f "$PUDDLE_ROOT/dotnet.refuse" ]; then
        echo "no trust for you" >&2
        exit 4
    fi
    ;;
*--trust*)
    # Under WSL the real one exits 4 here (it reaches for powershell.exe) though the guest's own
    # trust is done: the step must go by the check.
    [ -f "$PUDDLE_ROOT/dotnet.trust-exit4" ] && exit 4
    ;;
esac
exit 0
"#;

fn dev_cert() -> GuestDevCert {
    let dir = tempfile::tempdir().unwrap();
    let report = DevCertManager::new(dir.path(), Unsupported)
        .ensure(SystemTime::now())
        .unwrap();
    GuestDevCert::new(&report.material)
}

fn with_dev(dev: &GuestDevCert) -> GuestTrust {
    trust(&[], &[]).with_dev_cert(dev.clone())
}

fn has_openssl() -> bool {
    Command::new("openssl").arg("version").output().is_ok()
}

fn dotnet_log(g: &Guest) -> Vec<String> {
    std::fs::read_to_string(g.root.join("dotnet.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_development_certificate_reaches_the_guest_and_net_is_set_up_once_per_certificate() {
    assert!(has_openssl(), "the test host needs openssl");
    let dev = dev_cert();
    let t = with_dev(&dev);
    for sh in shells() {
        let g = Guest::new(&sh, "", true);
        write_exec(&g.root.join("stubs/dotnet"), DOTNET_STUB);
        std::fs::write(g.root.join("dotnet.trust-exit4"), "").unwrap();
        let out = g.boot(&t);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("certificate imported into .NET and trusted for its check"),
            "{stdout}"
        );

        // Files: the certificate is public, the key is root's alone, the system store has it.
        let cert = g.read("/etc/puddle/dev-cert/localhost.crt");
        assert_eq!(
            cert,
            g.read("/usr/local/share/ca-certificates/puddle-dev/localhost.crt")
        );
        assert!(
            g.read("/etc/puddle/dev-cert/localhost.key")
                .contains("BEGIN PRIVATE KEY")
        );
        let mode = |p: &str| std::fs::metadata(g.p(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("/etc/puddle/dev-cert/localhost.key"), 0o600);
        assert_eq!(mode("/etc/puddle/dev-cert/localhost.crt"), 0o644);
        // Trusted by the system store and by the bundle and Node's file.
        assert!(
            g.read("/etc/ssl/certs/ca-certificates.crt")
                .contains(&body(&cert)[..60])
        );
        assert!(g.read(CA_BUNDLE_PATH).contains(&body(&cert)[..60]));
        assert!(g.read(EXTRA_CAS_PATH).contains(&body(&cert)[..60]));
        let env = g.read(ENV_FILE_GUEST);
        for line in [
            "export DEV_CERT_PEM='/etc/puddle/dev-cert/localhost.crt'",
            "export DEV_KEY_PEM='/etc/puddle/dev-cert/localhost.key'",
            "export DOTNET_GENERATE_ASPNET_CERTIFICATE='false'",
        ] {
            assert!(env.contains(line), "{line} in {env}");
        }

        // .NET was asked in order, with its trust folder on SSL_CERT_DIR, and got a PKCS#12 that
        // opens and holds this very certificate; the file is gone afterwards.
        let log = dotnet_log(&g);
        let verbs: Vec<&str> = log.iter().map(|l| l.split(" | ").next().unwrap()).collect();
        assert_eq!(verbs.len(), 3, "{log:?}");
        assert!(
            verbs[0].starts_with("dev-certs https --clean --import /"),
            "{verbs:?}"
        );
        assert!(verbs[0].contains(" -p "), "{verbs:?}");
        assert_eq!(verbs[1], "dev-certs https --trust");
        assert_eq!(verbs[2], "dev-certs https --check --trust");
        assert!(
            log[0].contains("SSL_CERT_DIR=/root/.aspnet/dev-certs/trust:/etc/ssl/certs"),
            "{log:?}"
        );
        assert!(log[0].contains("GEN=false"), "{log:?}");
        assert_eq!(body(&g.read("/dotnet.import-cert")), body(&cert));
        let pfx_path = verbs[0].split(' ').nth(5).unwrap();
        assert!(
            !Path::new(pfx_path).exists(),
            "the import file is left behind at {pfx_path}"
        );

        // The key never reaches the boot output.
        let key_line = g
            .read("/etc/puddle/dev-cert/localhost.key")
            .lines()
            .nth(1)
            .unwrap()
            .to_owned();
        assert!(!String::from_utf8_lossy(&out.stdout).contains(&key_line));
        assert!(!String::from_utf8_lossy(&out.stderr).contains(&key_line));

        // The same certificate again: nothing is asked of .NET.
        let again = g.boot(&t);
        assert!(again.status.success(), "{}", show(&again));
        assert!(
            String::from_utf8_lossy(&again.stdout)
                .contains("certificate unchanged; .NET already has it")
        );
        assert_eq!(dotnet_log(&g).len(), 3);

        // A renewed certificate is imported at the next boot, and the old one leaves the stores.
        let renewed = dev_cert();
        let out = g.boot(&with_dev(&renewed));
        assert!(out.status.success(), "{}", show(&out));
        assert_eq!(dotnet_log(&g).len(), 6);
        let new_cert = g.read("/etc/puddle/dev-cert/localhost.crt");
        assert_ne!(new_cert, cert);
        assert_eq!(body(&g.read("/dotnet.import-cert")), body(&new_cert));
        assert!(!g.read(CA_BUNDLE_PATH).contains(&body(&cert)[..60]));
        assert!(
            !g.read("/etc/ssl/certs/ca-certificates.crt")
                .contains(&body(&cert)[..60])
        );
    }
}

#[test]
fn a_net_that_refuses_the_certificate_is_a_line_in_the_boot_output_and_is_tried_again() {
    assert!(has_openssl());
    let t = with_dev(&dev_cert());
    let g = Guest::new(&shells()[0], "", true);
    write_exec(&g.root.join("stubs/dotnet"), DOTNET_STUB);
    std::fs::write(g.root.join("dotnet.refuse"), "").unwrap();
    let out = g.boot(&t);
    assert!(
        out.status.success(),
        "the boot must not fail over .NET: {}",
        show(&out)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .contains(".NET refused it at 'dev-certs --check --trust' (exit 4): no trust for you"),
        "{stdout}"
    );
    // Everything else is in place.
    assert!(
        g.read(CA_BUNDLE_PATH)
            .contains(&body(&g.read("/etc/puddle/dev-cert/localhost.crt"))[..60])
    );
    // No record of success, so the next boot tries again, and then succeeds.
    std::fs::remove_file(g.root.join("dotnet.refuse")).unwrap();
    let out = g.boot(&t);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("imported into .NET"),
        "{}",
        show(&out)
    );
}

#[test]
fn an_image_without_net_or_openssl_keeps_the_certificate_and_says_so() {
    let t = with_dev(&dev_cert());
    // No dotnet on the PATH (the test host may well have one).
    let mut g = Guest::new(&shells()[0], "", true);
    let bin = g.root.join("without-dotnet");
    std::fs::create_dir_all(&bin).unwrap();
    for entry in std::fs::read_dir("/usr/bin").unwrap().flatten() {
        if entry.file_name() != "dotnet" {
            let _ = std::os::unix::fs::symlink(entry.path(), bin.join(entry.file_name()));
        }
    }
    g.path = format!("{}:{}", g.root.join("stubs").display(), bin.display());
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("certificate in place; no .NET SDK in this image")
    );
    assert!(g.p("/etc/puddle/dev-cert/localhost.key").exists());

    // dotnet without a working openssl: the import file can't be built.
    assert!(has_openssl());
    let g = Guest::new(&shells()[0], "", true);
    write_exec(&g.root.join("stubs/dotnet"), DOTNET_STUB);
    write_exec(
        &g.root.join("stubs/openssl"),
        "#!/bin/sh\necho 'openssl: broken' >&2\nexit 1\n",
    );
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout
            .contains(".NET not set up (openssl could not build the import file: openssl: broken)"),
        "{stdout}"
    );
    assert_eq!(
        dotnet_log(&g).len(),
        0,
        "dotnet was asked without an import file"
    );
}

#[test]
fn a_host_with_no_certificate_leaves_the_guest_without_one_and_drops_one_it_had() {
    assert!(has_openssl());
    let dev = dev_cert();
    let g = Guest::new(&shells()[0], "", true);
    write_exec(&g.root.join("stubs/dotnet"), DOTNET_STUB);
    assert!(g.boot(&with_dev(&dev)).status.success());
    assert!(g.p("/etc/puddle/dev-cert/localhost.key").exists());

    // The plan no longer lists the files: the boot removes them (the key first of all).
    let out = g.boot(&trust(&[], &[]));
    assert!(out.status.success(), "{}", show(&out));
    for gone in [
        "/etc/puddle/dev-cert/localhost.key",
        "/etc/puddle/dev-cert/localhost.crt",
        "/usr/local/share/ca-certificates/puddle-dev/localhost.crt",
    ] {
        assert!(!g.p(gone).exists(), "{gone}");
    }
    let fresh = Guest::new(&shells()[0], "", true);
    assert!(fresh.boot(&trust(&[], &[])).status.success());
    assert!(!fresh.p("/etc/puddle/dev-cert").exists());
}

// --- Java's cacerts (the plan's java-trust step, through the real boot.sh) -------------------------

/// Stands in for `keytool`: a store is a text file of aliases (the imported certificates are kept
/// next to it, `<store>.<alias>`), the password is `changeit` unless `<store>.password` says
/// otherwise, and an import fails while `<store>.failimport` exists. Every call is logged.
const KEYTOOL_STUB: &str = r#"#!/bin/sh
echo "$*" >>"$PUDDLE_ROOT/keytool.log"
op= store= pass= alias= file=
while [ $# -gt 0 ]; do
    case $1 in
    -list | -importcert | -delete) op=$1 ;;
    -keystore) store=$2; shift ;;
    -storepass) pass=$2; shift ;;
    -alias) alias=$2; shift ;;
    -file) file=$2; shift ;;
    esac
    shift
done
want=changeit
[ -f "$store.password" ] && want=$(cat "$store.password")
[ "$pass" = "$want" ] || { echo "keytool error: Keystore was tampered with, or password was incorrect" >&2; exit 1; }
case $op in
-list)
    echo "Keystore type: PKCS12"
    while read -r a; do
        echo "$a, Oct 28, 2026, trustedCertEntry,"
        echo "Certificate fingerprint (SHA-256): AA"
    done <"$store"
    ;;
-importcert)
    [ ! -f "$store.failimport" ] || { echo "keytool error: cannot import" >&2; exit 1; }
    grep -q -- '-----BEGIN CERTIFICATE-----' "$file" || exit 1
    echo "$alias" >>"$store"
    cp "$file" "$store.$alias"
    echo "Certificate was added to keystore"
    ;;
-delete)
    grep -vx "$alias" "$store" >"$store.new"
    mv "$store.new" "$store"
    ;;
esac
"#;

/// Where a fake JDK's `cacerts` is, relative to the guest root, for the layouts that exist.
const JDK17_STORE: &str = "/usr/lib/jvm/java-17/lib/security/cacerts";
const JDK8_STORE: &str = "/opt/jdk8/jre/lib/security/cacerts";

impl Guest {
    /// A JDK at `home` (`bin/keytool`, a store holding one distro alias).
    fn jdk(&self, home: &str, store: &str) {
        write_exec(&self.p(&format!("{home}/bin/keytool")), KEYTOOL_STUB);
        self.write(store, "distro-root\n");
    }

    /// How many keytool calls started with `op` (an empty `op` counts every call).
    fn keytool_calls(&self, op: &str) -> usize {
        std::fs::read_to_string(self.root.join("keytool.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.starts_with(op))
            .count()
    }

    /// The aliases in a store, in order.
    fn aliases(&self, store: &str) -> Vec<String> {
        self.read(store).lines().map(str::to_owned).collect()
    }
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn an_image_with_java_gets_the_extra_cas_in_its_cacerts_next_to_the_distro_entries() {
    let (corp_der, corp) = root_pem("Corp Root");
    let ca = puddle_ca();
    let t = trust(&[&corp_der], &[&ca]);
    for sh in shells() {
        let g = Guest::new(&sh, "", false);
        g.jdk("/usr/lib/jvm/java-17", JDK17_STORE);
        let out = g.boot(&t);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        let stdout = stdout_of(&out);
        assert!(stdout.contains("updated, 2 added, 0 removed"), "{stdout}");
        let aliases = g.aliases(JDK17_STORE);
        assert_eq!(aliases[0], "distro-root", "the image's entries stay");
        assert_eq!(aliases.len(), 3);
        assert!(aliases[1..].iter().all(|a| a.starts_with("puddle-")));
        // What went in is the certificate itself, whole.
        let imported: Vec<String> = aliases[1..]
            .iter()
            .map(|a| g.read(&format!("{JDK17_STORE}.{a}")))
            .collect();
        assert!(imported.iter().any(|p| body(p) == body(ca.pem())));
        assert!(imported.iter().any(|p| body(p) == body(&corp)));
    }
}

#[test]
fn a_keytool_on_path_behind_a_symlink_finds_a_symlinked_store_and_a_jdk8_layout() {
    // Debian: /usr/bin/keytool is a link into the JDK, whose cacerts is a link to the system store.
    let ca = puddle_ca();
    let t = trust(&[], &[&ca]);
    let mut g = Guest::new(&shells()[0], "", false);
    write_exec(&g.p("/usr/lib/jvm/java-11/bin/keytool"), KEYTOOL_STUB);
    g.write("/etc/ssl/certs/java/cacerts", "distro-root\n");
    std::fs::create_dir_all(g.p("/usr/lib/jvm/java-11/lib/security")).unwrap();
    std::os::unix::fs::symlink(
        g.p("/etc/ssl/certs/java/cacerts"),
        g.p("/usr/lib/jvm/java-11/lib/security/cacerts"),
    )
    .unwrap();
    std::fs::create_dir_all(g.p("/usr/bin-java")).unwrap();
    std::os::unix::fs::symlink(
        g.p("/usr/lib/jvm/java-11/bin/keytool"),
        g.p("/usr/bin-java/keytool"),
    )
    .unwrap();
    // JDK 8 keeps its store below jre/; it is only reachable by the folder search.
    g.jdk("/opt/jdk8", JDK8_STORE);
    g.path = format!("{}:{}", g.p("/usr/bin-java").display(), g.path);
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(g.aliases("/etc/ssl/certs/java/cacerts").len(), 2);
    assert_eq!(g.aliases(JDK8_STORE).len(), 2);
    // The link on PATH and the folder search reach the same store once: two stores, two imports.
    assert_eq!(g.keytool_calls("-importcert"), 2);
}

#[test]
fn jdks_of_version_managers_are_found_and_one_in_an_unlisted_folder_is_the_documented_limit() {
    let (corp_der, _) = root_pem("Corp Root");
    let t = trust(&[&corp_der], &[&puddle_ca()]);
    let found = [
        "/root/.sdkman/candidates/java/21.0.4-tem",
        "/home/dev/.sdkman/candidates/java/17.0.12-tem",
        "/usr/local/sdkman/candidates/java/11.0.24-tem",
        "/home/dev/.jdks/corretto-21",
        "/root/.asdf/installs/java/temurin-21",
        "/home/dev/.local/share/mise/installs/java/21",
        "/usr/java/jdk-17",
        "/usr/lib64/jvm/java-21",
        "/usr/local/java-17",
    ];
    for sh in shells() {
        let g = Guest::new(&sh, "", false);
        for home in found {
            g.jdk(home, &format!("{home}/lib/security/cacerts"));
        }
        // SDKMAN's `current` is a link to a version: one store, imported once.
        std::os::unix::fs::symlink(
            g.p("/root/.sdkman/candidates/java/21.0.4-tem"),
            g.p("/root/.sdkman/candidates/java/current"),
        )
        .unwrap();
        // Not on PATH, not in JAVA_HOME, not in a listed folder: not found.
        g.jdk(
            "/srv/tools/jdk-21",
            "/srv/tools/jdk-21/lib/security/cacerts",
        );
        let out = g.boot(&t);
        assert!(out.status.success(), "{}: {}", sh.display(), show(&out));
        for home in found {
            let store = format!("{home}/lib/security/cacerts");
            let aliases = g.aliases(&store);
            assert_eq!(aliases.len(), 3, "{home}: {}", stdout_of(&out));
            assert_eq!(aliases[0], "distro-root");
            assert!(
                aliases[1..].iter().all(|a| a.starts_with("puddle-")),
                "{aliases:?}"
            );
        }
        assert_eq!(
            g.aliases("/srv/tools/jdk-21/lib/security/cacerts"),
            ["distro-root"],
            "a JDK in an unlisted folder is left alone (the documented limit)"
        );
        assert_eq!(g.keytool_calls("-importcert"), found.len() * 2);
    }
}

#[test]
fn an_unchanged_boot_does_not_start_keytool_and_a_rebuilt_cacerts_is_filled_again() {
    let (corp_der, _) = root_pem("Corp Root");
    let t = trust(&[&corp_der], &[&puddle_ca()]);
    let g = Guest::new(&shells()[0], "", false);
    g.jdk("/usr/lib/jvm/java-17", JDK17_STORE);
    assert!(g.boot(&t).status.success());
    let calls = g.keytool_calls("");
    assert_eq!(calls, 3, "one list and two imports");
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    assert!(stdout_of(&out).contains("unchanged"));
    assert_eq!(g.keytool_calls(""), calls);
    // A Java package update rebuilds cacerts from the system store, without Puddle's entries.
    g.write(JDK17_STORE, "distro-root\nnew-distro-root\n");
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    assert!(stdout_of(&out).contains("2 added"));
    assert_eq!(g.aliases(JDK17_STORE).len(), 4);
}

#[test]
fn a_ca_that_left_the_extra_cas_leaves_the_cacerts_and_the_image_entries_stay() {
    let (a_der, _) = root_pem("Corp Root A");
    let (b_der, _) = root_pem("Corp Root B");
    let g = Guest::new(&shells()[0], "", false);
    g.jdk("/usr/lib/jvm/java-17", JDK17_STORE);
    assert!(g.boot(&trust(&[&a_der, &b_der], &[])).status.success());
    let before = g.aliases(JDK17_STORE);
    assert_eq!(before.len(), 3);
    let out = g.boot(&trust(&[&a_der], &[]));
    assert!(out.status.success(), "{}", show(&out));
    assert!(stdout_of(&out).contains("0 added, 1 removed"));
    let after = g.aliases(JDK17_STORE);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], "distro-root");
    assert!(before.contains(&after[1]));
    assert_eq!(g.keytool_calls("-delete"), 1);
}

#[test]
fn an_image_without_java_boots_and_starts_no_keytool() {
    let (corp_der, _) = root_pem("Corp Root");
    let g = Guest::new(&shells()[0], "", false);
    let out = g.boot(&trust(&[&corp_der], &[]));
    assert!(out.status.success(), "{}", show(&out));
    assert!(stdout_of(&out).contains("no Java in this image"));
    assert!(!g.root.join("keytool.log").exists());
    assert!(!g.p("/etc/puddle/java-trust.state").exists());
}

#[test]
fn a_store_with_another_password_is_skipped_and_the_boot_still_succeeds() {
    let (corp_der, _) = root_pem("Corp Root");
    let t = trust(&[&corp_der], &[]);
    let g = Guest::new(&shells()[0], "", false);
    g.jdk("/usr/lib/jvm/java-17", JDK17_STORE);
    g.write(&format!("{JDK17_STORE}.password"), "not-the-default");
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("cannot be listed with the default password, skipped"),
        "{stdout}"
    );
    assert_eq!(g.aliases(JDK17_STORE), ["distro-root"]);
    assert!(!g.p("/etc/puddle/java-trust.state").exists());
}

#[test]
fn a_failed_import_is_reported_not_recorded_and_retried_at_the_next_boot() {
    let (corp_der, _) = root_pem("Corp Root");
    let t = trust(&[&corp_der], &[]);
    let g = Guest::new(&shells()[0], "", false);
    g.jdk("/usr/lib/jvm/java-17", JDK17_STORE);
    g.write(&format!("{JDK17_STORE}.failimport"), "");
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    assert!(stdout_of(&out).contains("some changes failed, will retry at the next boot"));
    assert!(!g.p("/etc/puddle/java-trust.state").exists());
    std::fs::remove_file(g.p(&format!("{JDK17_STORE}.failimport"))).unwrap();
    let out = g.boot(&t);
    assert!(out.status.success(), "{}", show(&out));
    assert!(stdout_of(&out).contains("1 added"));
    assert_eq!(g.aliases(JDK17_STORE).len(), 2);
}
