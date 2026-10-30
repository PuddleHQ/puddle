// SPDX-License-Identifier: GPL-3.0-or-later
//! mitmproxy (`mitmdump`) as two corporate proxies: a TLS interceptor whose root is made for the
//! run, and a proxy that demands NTLM. Needs `PUDDLE_MITMDUMP_BIN`; with it unset `start` skips,
//! unless `PUDDLE_REQUIRE_MITMPROXY=1`, which makes a missing mitmproxy a failure (CI sets it).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{
    LOCAL, Prefix, READY, StartCost, StartSlot, configured_or_skip, read, require_file, stop,
    wait_ready,
};

/// The test root of one interceptor: made before the proxy starts and handed to it, so a test
/// can put it in a trust store (or leave it out) before any connection.
pub struct Root {
    pem: String,
    der: Vec<u8>,
    key_pem: String,
}

impl Root {
    /// A new self-signed CA named after `prefix`.
    ///
    /// # Panics
    /// When key generation fails.
    #[must_use]
    pub fn generate(prefix: &Prefix) -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            format!("Puddle lab interceptor root {}", prefix.as_str()),
        );
        let cert = params.self_signed(&key).unwrap();
        Self {
            pem: cert.pem(),
            der: cert.der().to_vec(),
            key_pem: key.serialize_pem(),
        }
    }

    /// The certificate, PEM.
    #[must_use]
    pub fn pem(&self) -> &str {
        &self.pem
    }

    /// The certificate, DER.
    #[must_use]
    pub fn der(&self) -> &[u8] {
        &self.der
    }
}

/// The split-DNS addon for `names`, each with the port that replaces the one asked for (`None`
/// keeps it).
fn split_dns(names: &[(&str, Option<u16>)]) -> String {
    let set = names
        .iter()
        .map(|(n, port)| {
            let port = port.map_or_else(|| "None".to_owned(), |p| p.to_string());
            format!("\"{n}\": {port}")
        })
        .collect::<Vec<_>>()
        .join(", ");
    include_str!("split_dns_addon.py").replace("{__NAMES__}", &format!("{{{set}}}"))
}

/// Names without a port of their own.
fn plain(names: &[&str]) -> Vec<(String, Option<u16>)> {
    names.iter().map(|n| ((*n).to_owned(), None)).collect()
}

fn borrowed(names: &[(String, Option<u16>)]) -> Vec<(&str, Option<u16>)> {
    names.iter().map(|(n, p)| (n.as_str(), *p)).collect()
}

/// The port in mitmproxy's `HTTP(S) proxy listening at 127.0.0.1:39551.` log line.
fn listening_port(log: &str) -> Option<u16> {
    let rest = log.split("proxy listening at ").nth(1)?;
    let line = rest.lines().next()?.trim_end_matches('.');
    line.rsplit(':').next()?.parse().ok()
}

/// Waits until `child` has logged the port it bound, and returns it.
///
/// # Panics
/// When the child exits first or logs nothing within `within`.
fn wait_listening(child: &mut Child, log_path: &Path, within: Duration) -> u16 {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(port) = listening_port(&read(log_path)) {
            return port;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("mitmproxy exited early ({status})\n{}", read(log_path));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("mitmproxy did not log its port\n{}", read(log_path));
}

/// A running `mitmdump`; stopped on drop.
struct Mitm {
    child: Child,
    port: u16,
    prefix: Prefix,
    dir: tempfile::TempDir,
    started: StartCost,
}

impl Mitm {
    /// `extra` are the mode-specific arguments; `root` is installed as the signing CA.
    fn spawn(kind: &str, root: Option<&Root>, extra: &[String], addons: &[String]) -> Option<Self> {
        let bin = std::env::var_os("PUDDLE_MITMDUMP_BIN");
        // A missing program is a skip (`None`); `configured_or_skip` panics instead when required.
        configured_or_skip(
            bin.is_some(),
            std::env::var_os("PUDDLE_REQUIRE_MITMPROXY").is_some(),
            "PUDDLE_REQUIRE_MITMPROXY",
            "set PUDDLE_MITMDUMP_BIN to run against mitmproxy",
        )
        .then_some(())?;
        let bin = bin.unwrap();
        require_file(&bin, "PUDDLE_MITMDUMP_BIN");
        let prefix = Prefix::new(kind);
        // Held until the proxy listens; the wait for it is not part of the start's cost.
        let slot = StartSlot::take(prefix.as_str());
        let begun = Instant::now();
        let dir = tempfile::Builder::new()
            .prefix(prefix.as_str())
            .tempdir()
            .unwrap();
        let confdir: PathBuf = dir.path().join("conf");
        fs::create_dir(&confdir).unwrap();
        if let Some(root) = root {
            // mitmproxy reads its signing CA from one file: the key, then the certificate.
            fs::write(
                confdir.join("mitmproxy-ca.pem"),
                format!("{}{}", root.key_pem, root.pem),
            )
            .unwrap();
        }
        let mut command = Command::new(bin);
        command
            // The log is read while the proxy runs; Python buffers a redirected stdout.
            .env("PYTHONUNBUFFERED", "1")
            // Port 0: mitmproxy binds the port itself and logs the one it got, so no other
            // process can take it between a pick and the bind.
            .args(["--listen-host", &LOCAL.to_string(), "--listen-port", "0"])
            .arg("--set")
            .arg(format!("confdir={}", confdir.display()))
            .args(["--set", "flow_detail=1", "--set", "termlog_verbosity=info"])
            // The origins in tests have certificates of their own roots; the interceptor's
            // upstream leg does not check them (a client's own check is what a test measures).
            .args(["--set", "ssl_insecure=true"]);
        for (n, addon) in addons.iter().enumerate() {
            let path = dir.path().join(format!("addon{n}.py"));
            fs::write(&path, addon).unwrap();
            command.arg("-s").arg(path);
        }
        let log = fs::File::create(dir.path().join("mitm.log")).unwrap();
        let mut child = command
            .args(extra)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let log_path = dir.path().join("mitm.log");
        let port = wait_listening(&mut child, &log_path, READY);
        wait_ready(port, Some(&mut child), READY, || read(&log_path));
        let started = StartCost::measure(&prefix, begun, Some(&child));
        drop(slot);
        Some(Self {
            child,
            port,
            prefix,
            dir,
            started,
        })
    }

    fn log(&self) -> String {
        // The log is a file the proxy flushes per line; a moment covers the last flow.
        std::thread::sleep(Duration::from_millis(300));
        read(&self.dir.path().join("mitm.log"))
    }
}

impl Drop for Mitm {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// A TLS-intercepting explicit proxy: every `CONNECT` is terminated and re-signed with the run's
/// [`Root`], as a corporate inspection proxy does.
pub struct Interceptor {
    mitm: Mitm,
    root: Root,
}

impl Interceptor {
    /// Starts the interceptor, or `None` (skip) without `PUDDLE_MITMDUMP_BIN`. `names` are
    /// resolved only by the interceptor, to loopback (split DNS: this host cannot resolve them).
    ///
    /// # Panics
    /// When mitmproxy is required but missing, or does not start.
    #[must_use]
    pub fn start(names: &[&str]) -> Option<Self> {
        Self::start_on_ports(&plain(names))
    }

    /// As [`Self::start`], with a port for each name: the interceptor connects the name there
    /// whatever port the client asked for (a decrypted Git host is always on 443, which a lab
    /// cannot bind).
    ///
    /// # Panics
    /// When mitmproxy is required but missing, or does not start.
    #[must_use]
    pub fn start_on_ports(names: &[(String, Option<u16>)]) -> Option<Self> {
        let root = Root::generate(&Prefix::new("root"));
        let addon = split_dns(&borrowed(names));
        let mitm = Mitm::spawn("interceptor", Some(&root), &[], &[addon])?;
        Some(Self { mitm, root })
    }

    /// The port the interceptor listens on (`127.0.0.1`).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.mitm.port
    }

    /// The root every intercepted certificate chains to.
    #[must_use]
    pub fn root(&self) -> &Root {
        &self.root
    }

    /// This fixture's per-run prefix.
    #[must_use]
    pub fn prefix(&self) -> &Prefix {
        &self.mitm.prefix
    }

    /// What the start cost, in wall-clock and CPU time.
    #[must_use]
    pub fn start_cost(&self) -> StartCost {
        self.mitm.started
    }

    /// The flow log: one line per intercepted request (`GET https://host/path ... 200`).
    #[must_use]
    pub fn log(&self) -> String {
        self.mitm.log()
    }
}

/// One line of the NTLM proxy's handshake log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NtlmLeg {
    /// The proxy demanded NTLM (no usable token yet).
    Demand {
        /// The client connection (first 8 characters of mitmproxy's id).
        conn: String,
    },
    /// The client sent a Type 1 message and got the proxy's challenge.
    Challenge {
        /// The client connection.
        conn: String,
    },
    /// The client sent a Type 3 message and was accepted.
    Accepted {
        /// The client connection.
        conn: String,
        /// `DOMAIN\user` from the Type 3 message.
        user: String,
        /// Whether the response was NTLMv2-sized.
        ntlmv2: bool,
    },
}

/// A proxy that answers `407` with an NTLM challenge and accepts any well-formed Type 3. It tests
/// the protocol (three legs on one connection), not the identity. Tunnels are not intercepted.
pub struct NtlmProxy {
    mitm: Mitm,
}

impl NtlmProxy {
    /// Starts the proxy, or `None` (skip) without `PUDDLE_MITMDUMP_BIN`. `names` are resolved
    /// only by the proxy, to loopback.
    ///
    /// # Panics
    /// When mitmproxy is required but missing, or does not start.
    #[must_use]
    pub fn start(names: &[&str]) -> Option<Self> {
        Self::start_on_ports(&plain(names))
    }

    /// As [`Self::start`], with a port for each name (see [`Interceptor::start_on_ports`]).
    ///
    /// # Panics
    /// When mitmproxy is required but missing, or does not start.
    #[must_use]
    pub fn start_on_ports(names: &[(String, Option<u16>)]) -> Option<Self> {
        Mitm::spawn(
            "ntlm",
            None,
            &["--ignore-hosts".into(), ".*".into()],
            &[
                include_str!("ntlm_proxy_addon.py").into(),
                split_dns(&borrowed(names)),
            ],
        )
        .map(|mitm| Self { mitm })
    }

    /// The port the proxy listens on (`127.0.0.1`).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.mitm.port
    }

    /// This fixture's per-run prefix.
    #[must_use]
    pub fn prefix(&self) -> &Prefix {
        &self.mitm.prefix
    }

    /// What the start cost, in wall-clock and CPU time.
    #[must_use]
    pub fn start_cost(&self) -> StartCost {
        self.mitm.started
    }

    /// The proxy's log.
    #[must_use]
    pub fn log(&self) -> String {
        self.mitm.log()
    }

    /// The handshake legs seen so far, in order.
    #[must_use]
    pub fn legs(&self) -> Vec<NtlmLeg> {
        self.mitm.log().lines().filter_map(parse_leg).collect()
    }
}

fn parse_leg(line: &str) -> Option<NtlmLeg> {
    let rest = &line[line.find("NTLM ")? + 5..];
    let mut words = rest.split_whitespace();
    let kind = words.next()?;
    let conn = words.next()?.to_owned();
    match kind {
        "leg1" => Some(NtlmLeg::Challenge { conn }),
        "leg3" => {
            let user = rest
                .split("user=")
                .nth(1)?
                .split_whitespace()
                .next()?
                .to_owned();
            Some(NtlmLeg::Accepted {
                conn,
                user,
                ntlmv2: rest.contains("ntlmv2=True"),
            })
        }
        "407" => Some(NtlmLeg::Demand { conn }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_certificate_is_pem() {
        assert!(
            Root::generate(&Prefix::new("t"))
                .pem()
                .starts_with("-----BEGIN CERTIFICATE-----")
        );
    }

    #[test]
    fn the_port_mitmproxy_bound_is_read_from_its_log() {
        let log = "[11:29:01.892] HTTP(S) proxy listening at 127.0.0.1:39551.\n[11:29:02] more";
        assert_eq!(listening_port(log), Some(39551));
        assert_eq!(listening_port("[11:29:01.892] loading addons"), None);
        assert_eq!(listening_port("proxy listening at nowhere."), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_logged_port_is_returned_while_the_child_runs() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        fs::write(&log, "HTTP(S) proxy listening at 127.0.0.1:4242.\n").unwrap();
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        let port = wait_listening(&mut child, &log, Duration::from_secs(5));
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(port, 4242);
    }

    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "mitmproxy exited early")]
    fn a_mitmproxy_that_exits_before_logging_its_port_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        fs::write(&log, "Error logged during startup, exiting...\n").unwrap();
        let mut child = Command::new("true").spawn().unwrap();
        wait_listening(&mut child, &log, Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    #[should_panic(expected = "mitmproxy did not log its port")]
    fn a_mitmproxy_that_logs_no_port_in_time_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        // The panic skips any cleanup; the child ends by itself after a second.
        let mut child = Command::new("sleep").arg("1").spawn().unwrap();
        wait_listening(&mut child, &log, Duration::from_millis(200));
    }

    #[test]
    fn an_interceptor_binds_its_own_port_and_nobody_else_can_take_it() {
        // `start` panics when `PUDDLE_REQUIRE_MITMPROXY` is set (CI) and prints `skipped:` and
        // gives `None` otherwise, so a host without mitmdump skips visibly and CI cannot.
        let Some(interceptor) = Interceptor::start(&[]) else {
            return;
        };
        let port = interceptor.port();
        assert_ne!(port, 0);
        let taken = std::net::TcpListener::bind((LOCAL, port)).unwrap_err();
        assert_eq!(taken.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn handshake_log_lines_parse_into_legs() {
        let lines = [
            "[10:00:00.1] NTLM 407 a1b2c3d4 echo.corp.test",
            "[10:00:00.2] NTLM leg1 a1b2c3d4 -> challenge TlRMTVNTUAAC",
            "[10:00:00.3] NTLM leg3 a1b2c3d4 accepted user=LAB\\alice ntlmv2=True",
            "[10:00:00.4] unrelated line",
            "[10:00:00.5] NTLM leg9 a1b2c3d4 -> something new",
        ];
        let legs: Vec<_> = lines.iter().filter_map(|l| parse_leg(l)).collect();
        assert_eq!(
            legs,
            vec![
                NtlmLeg::Demand {
                    conn: "a1b2c3d4".into()
                },
                NtlmLeg::Challenge {
                    conn: "a1b2c3d4".into()
                },
                NtlmLeg::Accepted {
                    conn: "a1b2c3d4".into(),
                    user: "LAB\\alice".into(),
                    ntlmv2: true
                },
            ]
        );
    }
}
