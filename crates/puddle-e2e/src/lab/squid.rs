// SPDX-License-Identifier: AGPL-3.0-or-later
//! A real Squid as an explicit corporate proxy, open or with Basic authentication.
//!
//! Started from `PUDDLE_SQUID_BIN` (a `squid` binary, as on the CI runner) or
//! `PUDDLE_SQUID_DOCKER` (an image such as `ubuntu/squid`, run with host networking, as in a local
//! lab). With neither set `start` skips, unless `PUDDLE_REQUIRE_SQUID=1`, which makes a missing
//! Squid a failure (CI sets it).

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{
    BASIC_USER, LOCAL, Prefix, READY, StartCost, basic_password, configured_or_skip, free_port,
    is_address_in_use, on_free_port, read, require_file, stop, try_answers,
};

/// How long [`Squid::access_log_with`] waits for its line (a loaded machine needs seconds).
const LOG_LINE_WAIT: Duration = Duration::from_secs(20);

/// What a lab Squid is set up to do.
#[derive(Clone, Debug)]
pub struct SquidOptions {
    /// Ports `CONNECT` and plain requests may reach; everything else is denied (as a corporate
    /// proxy denies odd ports).
    pub allowed_ports: Vec<u16>,
    /// Names only this proxy resolves (split DNS), each mapped to `127.0.0.1` in its hosts file.
    pub names: Vec<String>,
    /// Whether Basic authentication (`BASIC_USER`, `basic_password()`) is required.
    pub basic_auth: bool,
    /// The port of another proxy on loopback that every tunnel goes through instead of going
    /// direct (a corporate proxy's own parent): it is how a lab Squid reaches a name that only
    /// that proxy knows, on a port the hosts file cannot change.
    pub parent_port: Option<u16>,
}

impl SquidOptions {
    /// Basic authentication required, `*.corp.test` names `echo` and `web` resolvable.
    #[must_use]
    pub fn basic(allowed_ports: &[u16]) -> Self {
        Self {
            allowed_ports: allowed_ports.to_vec(),
            names: vec!["echo.corp.test".into(), "web.corp.test".into()],
            basic_auth: true,
            parent_port: None,
        }
    }

    /// Sends every tunnel through the proxy on loopback port `port` (see [`Self::parent_port`]).
    #[must_use]
    pub fn through(mut self, port: u16) -> Self {
        self.parent_port = Some(port);
        self
    }

    /// The same without authentication.
    #[must_use]
    pub fn open(allowed_ports: &[u16]) -> Self {
        Self {
            basic_auth: false,
            ..Self::basic(allowed_ports)
        }
    }
}

/// A running Squid; stopped on drop.
pub struct Squid {
    port: u16,
    prefix: Prefix,
    dir: tempfile::TempDir,
    stop: Stop,
    started: StartCost,
}

enum Stop {
    Process(Child),
    Container(String),
}

impl Drop for Squid {
    fn drop(&mut self) {
        match &mut self.stop {
            Stop::Process(child) => {
                stop(child);
                // A killed Squid cannot unlink its shared-memory segments.
                remove_shared_memory(&instance_name(self.port));
            }
            Stop::Container(name) => {
                let _ = Command::new("docker")
                    .args(["rm", "-f", name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

/// Squid's instance name (`-n`, alphanumeric only). It prefixes the shared-memory segments
/// (`/dev/shm/<name>-cf__*`) that Squid creates exclusively, so concurrent Squids, or one killed
/// earlier, would otherwise collide on the default name.
fn instance_name(port: u16) -> String {
    format!("puddletest{port}")
}

/// Removes the shared-memory segments left under `name` (Linux's `/dev/shm`; absent elsewhere).
fn remove_shared_memory(name: &str) {
    let prefix = format!("{name}-cf__");
    let Ok(entries) = fs::read_dir("/dev/shm") else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Writes Squid's configuration, hosts file and Basic helper into `dir`; `seen` is where Squid
/// sees that directory.
fn write_config(dir: &Path, seen: &Path, options: &SquidOptions, port: u16) {
    let helper = dir.join("basic-helper.sh");
    let password = basic_password();
    puddle_test_kit::write_executable(
        &helper,
        &format!(
            "#!/bin/sh\nwhile read -r user pass; do\n  if [ \"$user\" = \"{BASIC_USER}\" ] && [ \"$pass\" = \"{password}\" ]; then echo OK; else echo ERR; fi\ndone\n"
        ),
    );
    fs::write(
        dir.join("hosts"),
        format!("127.0.0.1 {}\n", options.names.join(" ")),
    )
    .unwrap();
    let at = |name: &str| seen.join(name).display().to_string();
    let ports = options
        .allowed_ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    let (auth_params, access) = if options.basic_auth {
        (
            format!(
                "auth_param basic program /bin/sh {}\n\
                 auth_param basic realm puddle-upstream-lab\n\
                 auth_param basic children 2\n\
                 acl authed proxy_auth REQUIRED\n",
                at("basic-helper.sh")
            ),
            "http_access allow authed",
        )
    } else {
        (String::new(), "http_access allow all")
    };
    let parent = options.parent_port.map_or_else(String::new, |parent| {
        format!("cache_peer {LOCAL} parent {parent} 0 no-query no-digest\nnever_direct allow all\n")
    });
    fs::write(
        dir.join("squid.conf"),
        format!(
            "http_port {LOCAL}:{port}\n\
             {auth_params}\
             acl SSL_ports port {ports}\n\
             acl Safe_ports port {ports}\n\
             acl CONNECT method CONNECT\n\
             http_access deny !Safe_ports\n\
             http_access deny CONNECT !SSL_ports\n\
             {access}\n\
             http_access deny all\n\
             hosts_file {hosts}\n\
             {parent}\
             cache deny all\n\
             buffered_logs off\n\
             acl readiness_probe urlpath_regex ^/{PROBE_PATH}$\n\
             access_log daemon:{access_log} squid !readiness_probe\n\
             cache_log stdio:{cache}\n\
             pid_filename {pid}\n\
             coredump_dir {core}\n\
             visible_hostname puddle-lab\n",
            hosts = at("hosts"),
            access_log = at("access.log"),
            cache = at("cache.log"),
            pid = at("squid.pid"),
            core = seen.display(),
        ),
    )
    .unwrap();
}

/// The path of the request that [`squid_answers`] sends; Squid does not log it.
const PROBE_PATH: &str = "puddle-lab-ready";

/// Whether Squid itself answers on `port`. A port that merely accepts proves nothing: a stranger
/// holding the port Squid failed to bind accepts too, and when it lets go the next connect is
/// refused. The probe asks for a URL on a port no lab Squid allows, so Squid denies it at once
/// without reaching out, and only Squid's error page carries its `Server: squid` or
/// `X-Squid-Error` header.
fn squid_answers(port: u16) -> bool {
    let probe = || -> std::io::Result<String> {
        let addr = std::net::SocketAddr::from((LOCAL, port));
        let mut conn = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1))?;
        let timeout = Some(Duration::from_secs(2));
        conn.set_read_timeout(timeout)?;
        conn.set_write_timeout(timeout)?;
        conn.write_all(format!("GET http://{LOCAL}:1/{PROBE_PATH} HTTP/1.0\r\n\r\n").as_bytes())?;
        // HTTP/1.0: Squid closes after its answer. A stranger that stays silent ends on the
        // read timeout, which is an error and so not an answer.
        let mut answer = Vec::new();
        conn.read_to_end(&mut answer)?;
        Ok(String::from_utf8_lossy(&answer).to_ascii_lowercase())
    };
    probe().is_ok_and(|head| {
        head.starts_with("http/")
            && (head.contains("\r\nserver: squid") || head.contains("\r\nx-squid-error:"))
    })
}

impl Squid {
    /// Starts Squid, or `None` (skip) when no Squid is configured; see the module docs.
    ///
    /// # Panics
    /// When Squid is required but missing, or does not start.
    #[must_use]
    pub fn start(options: &SquidOptions) -> Option<Self> {
        let bin = std::env::var_os("PUDDLE_SQUID_BIN");
        let image = std::env::var("PUDDLE_SQUID_DOCKER").ok();
        // A missing program is a skip (`None`); `configured_or_skip` panics instead when required.
        configured_or_skip(
            bin.is_some() || image.is_some(),
            std::env::var_os("PUDDLE_REQUIRE_SQUID").is_some(),
            "PUDDLE_REQUIRE_SQUID",
            "set PUDDLE_SQUID_BIN or PUDDLE_SQUID_DOCKER to run against Squid",
        )
        .then_some(())?;
        // Squid reads its port from its config file and takes no inherited socket, so the port
        // cannot be held across the start; a collision retries on a fresh port.
        Some(on_free_port(free_port, |port| {
            Self::launch(bin.as_deref(), image.as_deref(), options, port)
        }))
    }

    /// One start attempt on `port`. `Err` is Squid's own log when it did not come up.
    fn launch(
        bin: Option<&std::ffi::OsStr>,
        image: Option<&str>,
        options: &SquidOptions,
        port: u16,
    ) -> Result<Self, String> {
        let begun = Instant::now();
        let prefix = Prefix::new("squid");
        let dir = tempfile::Builder::new()
            .prefix(prefix.as_str())
            .tempdir()
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        // Where Squid sees the directory: here, or at /lab in the container.
        let seen = if image.is_some() {
            PathBuf::from("/lab")
        } else {
            dir.path().to_path_buf()
        };
        write_config(dir.path(), &seen, options, port);
        let stop = if let Some(image) = image {
            let name = format!("{}-{port}", prefix.as_str());
            let status = Command::new("docker")
                .args(["run", "-d", "--name", &name, "--network", "host", "-v"])
                .arg(format!("{}:/lab", dir.path().display()))
                .args([image, "-f", "/lab/squid.conf", "-NYC"])
                .stdout(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "docker run failed");
            Stop::Container(name)
        } else {
            let bin = bin.unwrap();
            require_file(bin, "PUDDLE_SQUID_BIN");
            let log = fs::File::create(dir.path().join("squid.stderr")).unwrap();
            let name = instance_name(port);
            remove_shared_memory(&name);
            let child = Command::new(bin)
                .arg("-f")
                .arg(dir.path().join("squid.conf"))
                .args(["-N", "-Y", "-C", "-n", &name])
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap();
            Stop::Process(child)
        };
        // A failed attempt drops `squid`, which stops it and removes its directory.
        let mut squid = Self {
            port,
            prefix,
            dir,
            stop,
            started: StartCost::default(),
        };
        squid.wait()?;
        let process = match &squid.stop {
            Stop::Process(child) => Some(child),
            Stop::Container(_) => None,
        };
        squid.started = StartCost::measure(&squid.prefix, begun, process);
        Ok(squid)
    }

    fn wait(&mut self) -> Result<(), String> {
        let dir = self.dir.path().to_path_buf();
        let container = match &self.stop {
            Stop::Container(name) => Some(name.clone()),
            Stop::Process(_) => None,
        };
        let child = match &mut self.stop {
            Stop::Process(child) => Some(child),
            Stop::Container(_) => None,
        };
        let diagnostics = || {
            let container_log = container.as_ref().map_or_else(String::new, |name| {
                let out = Command::new("docker").args(["logs", name]).output();
                out.map(|o| {
                    format!(
                        "docker logs:\n{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    )
                })
                .unwrap_or_default()
            });
            format!(
                "squid.stderr:\n{}\ncache.log:\n{}\n{container_log}",
                read(&dir.join("squid.stderr")),
                read(&dir.join("cache.log"))
            )
        };
        let port = self.port;
        let logs = [dir.join("squid.stderr"), dir.join("cache.log")];
        try_answers(
            child,
            READY,
            &diagnostics,
            || squid_answers(port),
            // A child that dies right after something else answered is a collision too.
            Duration::from_millis(100),
            // Squid exits on a failed bind, but a container's exit is only seen below: its log
            // names the cause at once.
            || logs.iter().any(|log| is_address_in_use(&read(log))),
        )?;
        if let Some(name) = &container {
            // Something else may answer on a taken port while the container's Squid has exited.
            let running = Command::new("docker")
                .args(["inspect", "-f", "{{.State.Running}}", name])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true");
            if !running {
                return Err(format!("the fixture exited early\n{}", diagnostics()));
            }
        }
        Ok(())
    }

    /// The port Squid listens on (`127.0.0.1`).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// This fixture's per-run prefix.
    #[must_use]
    pub fn prefix(&self) -> &Prefix {
        &self.prefix
    }

    /// What the start cost, from the call to the first accepted connection.
    #[must_use]
    pub fn start_cost(&self) -> StartCost {
        self.started
    }

    /// Squid's access log as it is now (what it forwarded, and as which user). Squid writes a
    /// tunnel's line only when the tunnel ends, so a test that expects a line uses
    /// [`Squid::access_log_with`], which waits for it.
    #[must_use]
    pub fn access_log(&self) -> String {
        let log = match &self.stop {
            // The container's Squid writes its logs as its own user, mode 0640.
            Stop::Container(name) => Command::new("docker")
                .args(["exec", name, "cat", "/lab/access.log"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default(),
            Stop::Process(_) => read(&self.dir.path().join("access.log")),
        };
        if log.is_empty() {
            let cache = read(&self.dir.path().join("cache.log"));
            let files: Vec<_> = fs::read_dir(self.dir.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            return format!("(empty access log; files {files:?}; cache.log:\n{cache})");
        }
        log
    }

    /// Squid's access log once a line containing `needle` is in it; the log as it stands if none
    /// shows within `LOG_LINE_WAIT` (the caller's assertion then fails with it). Squid logs a
    /// tunnel when it ends and its log daemon writes the line after that, so reading right after
    /// the client closed can find nothing yet.
    #[must_use]
    pub fn access_log_with(&self, needle: &str) -> String {
        self.access_log_within(needle, LOG_LINE_WAIT)
    }

    fn access_log_within(&self, needle: &str, wait: Duration) -> String {
        let deadline = Instant::now() + wait;
        // flake-ok: returns the log as it stands when the line never comes, so the caller's assertion
        // prints it; the kit's waits would panic without it.
        loop {
            let log = self.access_log();
            if log.contains(needle) || Instant::now() >= deadline {
                return log;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[cfg(test)]
mod tests;
