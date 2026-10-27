// SPDX-License-Identifier: GPL-3.0-or-later
//! A real Squid as an explicit corporate proxy, open or with Basic authentication.
//!
//! Started from `PUDDLE_SQUID_BIN` (a `squid` binary, as on the CI runner) or
//! `PUDDLE_SQUID_DOCKER` (an image such as `ubuntu/squid`, run with host networking, as in a local
//! lab). With neither set `start` skips, unless `PUDDLE_REQUIRE_SQUID=1`, which makes a missing
//! Squid a failure (CI sets it).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::{
    BASIC_PASSWORD, BASIC_USER, LOCAL, Prefix, READY, StartCost, configured_or_skip, free_port,
    on_free_port, read, require_file, stop, try_ready,
};

/// What a lab Squid is set up to do.
#[derive(Clone, Debug)]
pub struct SquidOptions {
    /// Ports `CONNECT` and plain requests may reach; everything else is denied (as a corporate
    /// proxy denies odd ports).
    pub allowed_ports: Vec<u16>,
    /// Names only this proxy resolves (split DNS), each mapped to `127.0.0.1` in its hosts file.
    pub names: Vec<String>,
    /// Whether Basic authentication (`BASIC_USER`, `BASIC_PASSWORD`) is required.
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
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\nwhile read -r user pass; do\n  if [ \"$user\" = \"{BASIC_USER}\" ] && [ \"$pass\" = \"{BASIC_PASSWORD}\" ]; then echo OK; else echo ERR; fi\ndone\n"
        ),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
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
             access_log daemon:{access_log}\n\
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
        try_ready(self.port, child, READY, diagnostics)?;
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

    /// Squid's access log (what it forwarded, and as which user).
    #[must_use]
    pub fn access_log(&self) -> String {
        // Squid flushes stdio logs lazily; give it a moment.
        std::thread::sleep(Duration::from_millis(300));
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Squid` around a process that is not Squid, in an empty directory.
    fn stand_in(command: &mut Command) -> Squid {
        Squid {
            port: free_port(),
            prefix: Prefix::new("squid"),
            dir: tempfile::tempdir().unwrap(),
            stop: Stop::Process(command.stdout(Stdio::null()).spawn().unwrap()),
            started: StartCost::default(),
        }
    }

    #[test]
    fn an_empty_access_log_is_explained_with_the_directory_and_cache_log() {
        let squid = stand_in(Command::new("sleep").arg("60"));
        fs::write(squid.dir.path().join("cache.log"), "cache says hi").unwrap();
        let log = squid.access_log();
        assert!(log.starts_with("(empty access log; files "), "{log}");
        assert!(log.contains("cache says hi"), "{log}");
    }

    #[test]
    fn a_squid_with_a_parent_proxy_sends_every_tunnel_to_it() {
        let dir = tempfile::tempdir().unwrap();
        write_config(
            dir.path(),
            dir.path(),
            &SquidOptions::open(&[443]).through(4242),
            3128,
        );
        let conf = read(&dir.path().join("squid.conf"));
        assert!(
            conf.contains(
                "cache_peer 127.0.0.1 parent 4242 0 no-query no-digest\nnever_direct allow all\n"
            ),
            "{conf}"
        );
        write_config(dir.path(), dir.path(), &SquidOptions::open(&[443]), 3128);
        assert!(!read(&dir.path().join("squid.conf")).contains("cache_peer"));
    }

    #[test]
    fn a_written_access_log_is_returned_as_is() {
        let squid = stand_in(Command::new("sleep").arg("60"));
        fs::write(squid.dir.path().join("access.log"), "GET x").unwrap();
        assert_eq!(squid.access_log(), "GET x");
    }

    #[test]
    #[should_panic(expected = "squid.stderr:\nboom\ncache.log:\ncache says hi")]
    fn a_squid_that_dies_at_start_shows_both_logs() {
        let mut squid = stand_in(&mut Command::new("false"));
        fs::write(squid.dir.path().join("squid.stderr"), "boom").unwrap();
        fs::write(squid.dir.path().join("cache.log"), "cache says hi").unwrap();
        let why = squid.wait().unwrap_err();
        panic!("{why}");
    }
}
