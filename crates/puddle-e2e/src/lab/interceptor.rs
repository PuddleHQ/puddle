// SPDX-License-Identifier: GPL-3.0-or-later
//! mitmproxy (`mitmdump`) as two corporate proxies: a TLS interceptor whose root is made for the
//! run, and a proxy that demands NTLM. Needs `PUDDLE_MITMDUMP_BIN`; with it unset `start` skips,
//! unless `PUDDLE_REQUIRE_MITMPROXY=1`, which makes a missing mitmproxy a failure (CI sets it).

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{LOCAL, Prefix, free_port, read, stop, wait_ready};

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
            format!("puddle lab interceptor root {}", prefix.as_str()),
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

/// The split-DNS addon for `names`.
fn split_dns(names: &[&str]) -> String {
    let set = names
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(", ");
    include_str!("split_dns_addon.py").replace("{__NAMES__}", &format!("{{{set}}}"))
}

/// A running `mitmdump`; stopped on drop.
struct Mitm {
    child: Child,
    port: u16,
    prefix: Prefix,
    dir: tempfile::TempDir,
    started_in: Duration,
}

impl Mitm {
    /// `extra` are the mode-specific arguments; `root` is installed as the signing CA.
    fn spawn(kind: &str, root: Option<&Root>, extra: &[String], addons: &[String]) -> Option<Self> {
        let Some(bin) = std::env::var_os("PUDDLE_MITMDUMP_BIN") else {
            assert!(
                std::env::var_os("PUDDLE_REQUIRE_MITMPROXY").is_none(),
                "PUDDLE_REQUIRE_MITMPROXY is set but PUDDLE_MITMDUMP_BIN is not"
            );
            eprintln!("skipped: set PUDDLE_MITMDUMP_BIN to run against mitmproxy");
            return None;
        };
        assert!(
            std::path::Path::new(&bin).is_file(),
            "PUDDLE_MITMDUMP_BIN={} is not a file: install mitmproxy or fix the path",
            bin.to_string_lossy()
        );
        let begun = Instant::now();
        let prefix = Prefix::new(kind);
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
        let port = free_port();
        let mut command = Command::new(bin);
        command
            // The log is read while the proxy runs; Python buffers a redirected stdout.
            .env("PYTHONUNBUFFERED", "1")
            .args(["--listen-host", &LOCAL.to_string(), "--listen-port"])
            .arg(port.to_string())
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
        wait_ready(port, Some(&mut child), || read(&log_path));
        Some(Self {
            child,
            port,
            prefix,
            dir,
            started_in: begun.elapsed(),
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
        let root = Root::generate(&Prefix::new("root"));
        let mitm = Mitm::spawn("interceptor", Some(&root), &[], &[split_dns(names)])?;
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

    /// How long the start took.
    #[must_use]
    pub fn started_in(&self) -> Duration {
        self.mitm.started_in
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
        let mitm = Mitm::spawn(
            "ntlm",
            None,
            &["--ignore-hosts".into(), ".*".into()],
            &[include_str!("ntlm_proxy_addon.py").into(), split_dns(names)],
        )?;
        Some(Self { mitm })
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

    /// How long the start took.
    #[must_use]
    pub fn started_in(&self) -> Duration {
        self.mitm.started_in
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
    fn handshake_log_lines_parse_into_legs() {
        let lines = [
            "[10:00:00.1] NTLM 407 a1b2c3d4 echo.corp.test",
            "[10:00:00.2] NTLM leg1 a1b2c3d4 -> challenge TlRMTVNTUAAC",
            "[10:00:00.3] NTLM leg3 a1b2c3d4 accepted user=LAB\\alice ntlmv2=True",
            "[10:00:00.4] unrelated line",
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
