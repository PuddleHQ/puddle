// SPDX-License-Identifier: AGPL-3.0-or-later
//! Settings in, [`GuestEnv`] and [`GuestFile`]s out.

use std::fmt::Write as _;
use std::net::SocketAddr;

use puddle_types::{GuestEnv, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec};

use crate::settings::{java_ip, loopback_entries};
use crate::{ConfigError, NoProxyEntry, ProxySettings};

/// apt's proxy drop-in. apt ignores `*_proxy` under `sudo` and in its `_apt`
/// workspace, so it gets its own file.
pub const APT_CONF_GUEST: &str = "/etc/apt/apt.conf.d/99puddle-proxy";

/// Keeps the proxy variables under `sudo` (whose `env_reset` drops them). Mode 0440, as sudo
/// requires.
pub const SUDOERS_GUEST: &str = "/etc/sudoers.d/puddle-proxy";

/// `ssh`'s system-wide drop-in (`/etc/ssh/ssh_config.d` is included by the distributions'
/// `ssh_config`). It comes before the rest of the system config, and a user's own `~/.ssh/config`
/// before it, so a `ProxyCommand` or `ProxyJump` the user sets wins.
pub const SSH_CONFIG_GUEST: &str = "/etc/ssh/ssh_config.d/00-puddle.conf";

/// The system-wide list of known host keys. `ssh` reads it, and so do tools that read no
/// `ssh_config` (cargo's built-in Git), so the keys of Puddle's SSH server for Git go here and
/// not into a file of Puddle's own.
pub const SSH_KNOWN_HOSTS_GUEST: &str = "/etc/ssh/ssh_known_hosts";

/// The system `ssh_config`, which has to read the folder above.
pub(crate) const SSH_CONFIG_SYSTEM: &str = "/etc/ssh/ssh_config";

/// What the system `ssh_config` has to include.
const SSH_CONFIG_INCLUDE: &str = "/etc/ssh/ssh_config.d/*.conf";

/// Maven settings with the proxy, passed as Maven's global settings through `MAVEN_ARGS`, so the
/// user's own `~/.m2/settings.xml` stays untouched and still applies on top.
pub const MAVEN_SETTINGS_GUEST: &str = "/etc/puddle/maven/settings.xml";

/// Gradle init script below the Gradle user home; sets the proxy system properties in every
/// build, so Gradle keeps the proxy when a tool clears `JAVA_TOOL_OPTIONS`.
pub const GRADLE_INIT_RELATIVE: &str = "init.d/puddle-proxy.gradle";

const SUDOERS_MODE: u32 = 0o440;

/// Mode of a Docker CLI config Puddle creates: `docker login` stores credentials in it later.
const DOCKER_CONFIG_MODE: u32 = 0o600;

/// Header of every generated file (the boot hook rewrites them at every boot).
const GENERATED: &str = "Written by Puddle at every boot; changes here are overwritten.";

/// Java's own `http.nonProxyHosts` default, repeated because setting the property replaces it.
const JDK_NON_PROXY_DEFAULTS: [&str; 5] = ["localhost", "127.*", "[::1]", "0.0.0.0", "[::0]"];

/// What the boot hook applies: the env (also given to the workspace spec for the exec/SSH path)
/// and the files, in a fixed order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestProxyConfig {
    /// Proxy variables for every process in the guest.
    pub env: GuestEnv,
    /// Tool config files.
    pub files: Vec<GuestFile>,
}

/// Builds the guest's proxy config from `settings` and the image's `ENV` (as
/// `ImageConfig::env`: `(name, value)` in image order, the last one wins).
///
/// The image env matters for three things: `JAVA_TOOL_OPTIONS` and `MAVEN_ARGS` keep the image's
/// value with Puddle's appended, and `GRADLE_USER_HOME` / `DOCKER_CONFIG` move the per-user
/// files. The image's own `*_PROXY` values are replaced: the agent is the guest's only route out.
///
/// ```
/// use puddle_guest_env::{ProxySettings, guest_proxy_config};
/// let config = guest_proxy_config(&ProxySettings::default(), &[]).unwrap();
/// assert_eq!(config.env.get("https_proxy"), Some("http://127.0.0.1:3128"));
/// assert_eq!(config.env.get("NO_PROXY"), Some("localhost,127.0.0.1,::1,172.17.0.1"));
/// assert!(config.files.iter().any(|f| f.path().as_str() == "/etc/apt/apt.conf.d/99puddle-proxy"));
/// ```
///
/// # Errors
///
/// [`ConfigError::ImageEnv`] when an image value that is passed on can't be an env value;
/// [`ConfigError::GuestPath`] when a per-user path built from `settings.home` is too long.
pub fn guest_proxy_config(
    settings: &ProxySettings,
    image_env: &[(String, String)],
) -> Result<GuestProxyConfig, ConfigError> {
    let image = |name: &str| {
        image_env
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let proxy_url = url(settings.proxy);
    let bypass = bypass_entries(settings);
    let no_proxy = join(bypass.iter().map(NoProxyEntry::as_str), ",");
    let java_bypass = java_non_proxy_hosts(settings);

    let mut env = Vars::default();
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
        env.set(name, &proxy_url);
    }
    env.set("NO_PROXY", &no_proxy);
    env.set("no_proxy", &no_proxy);
    // Node >= 22.21 / 24: built-in fetch and http(s) read the variables above (verified).
    env.set("NODE_USE_ENV_PROXY", "1");
    // Yarn Berry reads only its own variables.
    env.set("YARN_HTTP_PROXY", &proxy_url);
    env.set("YARN_HTTPS_PROXY", &proxy_url);
    // Electron's downloader (@electron/get) through global-agent.
    env.set("ELECTRON_GET_USE_PROXY", "1");
    env.set("GLOBAL_AGENT_HTTP_PROXY", &proxy_url);
    env.set("GLOBAL_AGENT_HTTPS_PROXY", &proxy_url);
    env.set("GLOBAL_AGENT_NO_PROXY", &no_proxy);
    // Java ignores *_proxy; every JVM reads this (Gradle, Maven's JVM, IDE servers).
    let java = format!(
        "-Dhttp.proxyHost={host} -Dhttp.proxyPort={port} -Dhttps.proxyHost={host} \
         -Dhttps.proxyPort={port} -Dhttp.nonProxyHosts={java_bypass}",
        host = java_ip(settings.proxy.ip()),
        port = settings.proxy.port(),
    );
    env.set(
        "JAVA_TOOL_OPTIONS",
        &appended(image("JAVA_TOOL_OPTIONS"), &java),
    );
    // Maven >= 3.9 reads MAVEN_ARGS; the settings file holds the <proxy> entries.
    env.set(
        "MAVEN_ARGS",
        &appended(image("MAVEN_ARGS"), &format!("-gs {MAVEN_SETTINGS_GUEST}")),
    );
    // Cargo fetches git dependencies with the git CLI, which follows the variables and, later,
    // ssh_config's ProxyCommand.
    env.set("CARGO_NET_GIT_FETCH_WITH_CLI", "true");
    if let Some(socket) = &settings.ssh_agent {
        env.set("SSH_AUTH_SOCK", socket.as_str());
    }
    let env = env.finish()?;

    let gradle_home = image_dir(image("GRADLE_USER_HOME"))
        .map_or_else(|| format!("{}/.gradle", home(settings)), str::to_owned);
    let mut files = vec![
        file(APT_CONF_GUEST, apt_conf(&proxy_url, &bypass))?,
        file(SUDOERS_GUEST, sudoers(&env))?
            .with_mode(SUDOERS_MODE)
            .map_err(|e| path_error(&e))?,
        file(
            MAVEN_SETTINGS_GUEST,
            maven_settings(settings.proxy, &java_bypass),
        )?,
        file(
            &format!("{gradle_home}/{GRADLE_INIT_RELATIVE}"),
            gradle_init(settings.proxy, &java_bypass),
        )?,
        file(SSH_CONFIG_GUEST, ssh_config(settings)?)?,
        ssh_config_include()?,
    ];
    if let Some(container) = settings.container_proxy {
        let docker_dir = image_dir(image("DOCKER_CONFIG"))
            .map_or_else(|| format!("{}/.docker", home(settings)), str::to_owned);
        let path =
            GuestPath::new(&format!("{docker_dir}/config.json")).map_err(|e| path_error(&e))?;
        files.push(
            GuestFile::merged(path, docker_cli_config(container, &no_proxy)?)
                .with_mode(DOCKER_CONFIG_MODE)
                .map_err(|e| path_error(&e))?,
        );
    }
    Ok(GuestProxyConfig { env, files })
}

/// The settings' home without a trailing `/` (only `/` itself has one).
fn home(settings: &ProxySettings) -> &str {
    settings.home.as_str().trim_end_matches('/')
}

/// An absolute, normalised directory from the image env, or `None` to use the default.
fn image_dir(value: Option<&str>) -> Option<&str> {
    value.filter(|v| *v != "/" && GuestPath::new(v).is_ok())
}

fn url(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

/// Loopback, the container gateway (the guest itself, seen from a container), then the user's.
fn bypass_entries(settings: &ProxySettings) -> Vec<NoProxyEntry> {
    let mut all: Vec<NoProxyEntry> = loopback_entries().into();
    if let Some(c) = settings.container_proxy {
        all.push(NoProxyEntry::ip(c.ip()));
    }
    for e in &settings.no_proxy {
        if !all.contains(e) {
            all.push(e.clone());
        }
    }
    all
}

fn java_non_proxy_hosts(settings: &ProxySettings) -> String {
    let mut all: Vec<String> = JDK_NON_PROXY_DEFAULTS.map(str::to_owned).into();
    let extra = settings
        .container_proxy
        .map(|c| NoProxyEntry::ip(c.ip()))
        .into_iter()
        .chain(settings.no_proxy.iter().cloned());
    for p in extra.flat_map(|e| e.java_patterns()) {
        if !all.contains(&p) {
            all.push(p);
        }
    }
    all.join("|")
}

fn join(items: impl Iterator<Item = String>, sep: &str) -> String {
    items.collect::<Vec<_>>().join(sep)
}

/// `ours` after the image's value, unless the image's value already carries it.
fn appended(image: Option<&str>, ours: &str) -> String {
    match image.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) if v.contains(ours) => v.to_owned(),
        Some(v) => format!("{v} {ours}"),
        None => ours.to_owned(),
    }
}

fn file(path: &str, contents: String) -> Result<GuestFile, ConfigError> {
    let path = GuestPath::new(path).map_err(|e| path_error(&e))?;
    Ok(GuestFile::new(path, contents.into_bytes()))
}

fn path_error(e: &puddle_types::ValidationError) -> ConfigError {
    ConfigError::GuestPath {
        reason: e.to_string(),
    }
}

/// Env under construction; the first invalid value is reported by [`Vars::finish`].
#[derive(Default)]
struct Vars {
    env: GuestEnv,
    error: Option<ConfigError>,
}

impl Vars {
    fn set(&mut self, name: &str, value: &str) {
        if let Err(e) = self.env.set(name, value)
            && self.error.is_none()
        {
            self.error = Some(ConfigError::ImageEnv {
                name: name.to_owned(),
                reason: e.reason().to_owned(),
            });
        }
    }

    fn finish(self) -> Result<GuestEnv, ConfigError> {
        self.error.map_or(Ok(self.env), Err)
    }
}

fn apt_conf(proxy_url: &str, bypass: &[NoProxyEntry]) -> String {
    let mut out = format!(
        "// {GENERATED}\n\
         // apt's downloads go through Puddle's proxy, whatever environment apt starts with.\n\
         Acquire::http::Proxy \"{proxy_url}\";\n\
         Acquire::https::Proxy \"{proxy_url}\";\n"
    );
    for host in bypass.iter().filter_map(NoProxyEntry::apt_host) {
        // Writing to a String can't fail.
        let _ = write!(
            out,
            "Acquire::http::Proxy::{host} \"DIRECT\";\nAcquire::https::Proxy::{host} \"DIRECT\";\n"
        );
    }
    out
}

fn sudoers(env: &GuestEnv) -> String {
    let names = join(env.iter().map(|(k, _)| k.to_owned()), " ");
    format!(
        "# {GENERATED}\n# Keeps Puddle's proxy variables under sudo.\nDefaults env_keep += \"{names}\"\n"
    )
}

fn maven_settings(proxy: SocketAddr, java_bypass: &str) -> String {
    let host = proxy.ip();
    let port = proxy.port();
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!-- {GENERATED}\n     \
         Maven's global settings through MAVEN_ARGS (-gs); ~/.m2/settings.xml still applies on top. -->\n\
         <settings xmlns=\"http://maven.apache.org/SETTINGS/1.0.0\"\n          \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n          \
         xsi:schemaLocation=\"http://maven.apache.org/SETTINGS/1.0.0 https://maven.apache.org/xsd/settings-1.0.0.xsd\">\n  \
         <proxies>\n"
    );
    for protocol in ["http", "https"] {
        // Writing to a String can't fail.
        let _ = write!(
            out,
            "    <proxy>\n      \
             <id>puddle-{protocol}</id>\n      \
             <active>true</active>\n      \
             <protocol>{protocol}</protocol>\n      \
             <host>{host}</host>\n      \
             <port>{port}</port>\n      \
             <nonProxyHosts>{java_bypass}</nonProxyHosts>\n    \
             </proxy>\n"
        );
    }
    out.push_str("  </proxies>\n</settings>\n");
    out
}

fn gradle_init(proxy: SocketAddr, java_bypass: &str) -> String {
    let host = java_ip(proxy.ip());
    let port = proxy.port();
    format!(
        "// {GENERATED}\n\
         // Gradle keeps Puddle's proxy even when JAVA_TOOL_OPTIONS is cleared.\n\
         [\n    \
         'http.proxyHost': '{host}',\n    \
         'http.proxyPort': '{port}',\n    \
         'https.proxyHost': '{host}',\n    \
         'https.proxyPort': '{port}',\n    \
         'http.nonProxyHosts': '{java_bypass}',\n    \
         'https.nonProxyHosts': '{java_bypass}'\n\
         ].each {{ key, value -> System.setProperty(key, value) }}\n"
    )
}

/// The one line that makes `ssh` read [`SSH_CONFIG_GUEST`]'s folder, for an image whose
/// `ssh_config` doesn't include it (Debian's does). The file is the image's, so it is merged,
/// not replaced: Puddle adds the line when it is missing and touches nothing else.
fn ssh_config_include() -> Result<GuestFile, ConfigError> {
    let spec = MergeSpec::new(
        MergeFormat::SshConfig,
        vec![MergeEntry::ssh_include(SSH_CONFIG_INCLUDE)],
    )
    .map_err(|e| path_error(&e))?;
    let path = GuestPath::new(SSH_CONFIG_SYSTEM).map_err(|e| path_error(&e))?;
    Ok(GuestFile::merged(path, spec))
}

/// The guest's `ssh_known_hosts` with the host keys of Puddle's SSH server for Git: `lines` are
/// `host keytype base64` entries, one per line. The file is the image's and the user's, so it is
/// merged: Puddle owns only the lines between its two marker lines.
///
/// # Errors
///
/// [`ConfigError::GuestPath`] when `lines` is empty, longer than the merge allows or holds a
/// comment, a blank line or a control character.
pub fn ssh_known_hosts_file(lines: &str) -> Result<GuestFile, ConfigError> {
    let spec = MergeSpec::new(
        MergeFormat::KnownHosts,
        vec![MergeEntry::known_hosts(lines)],
    )
    .map_err(|e| path_error(&e))?;
    let path = GuestPath::new(SSH_KNOWN_HOSTS_GUEST).map_err(|e| path_error(&e))?;
    Ok(GuestFile::merged(path, spec))
}

/// Every outbound `ssh` connection goes through the agent, which says that SSH is not supported
/// yet. What is inside the workspace is not outbound: a host named `localhost` or `::1` stays
/// ssh's own, and the agent dials any other destination itself when every address it resolves to
/// is loopback or inside the container bridge's /16 (`ssh 127.0.0.1`, a container running an SSH
/// server). Those are decided by address, never by name: a DNS name that starts with `127.` is
/// an ordinary destination.
fn ssh_config(settings: &ProxySettings) -> Result<String, ConfigError> {
    let agent = settings.agent.as_str();
    if agent.contains('%') || agent.chars().any(char::is_control) {
        return Err(ConfigError::GuestPath {
            reason: format!("the agent path {agent:?} can't be a ProxyCommand"),
        });
    }
    let mut command = format!("'{}' connect", agent.replace('\'', "'\\''"));
    if let Some(SocketAddr::V4(bridge)) = settings.container_proxy {
        let [a, b, ..] = bridge.ip().octets();
        let _ = write!(command, " --local-net {a}.{b}.0.0/16");
    }
    let mut config = format!(
        "# {GENERATED}\n\
         # SSH from a workspace is not supported yet: ssh runs the agent as its ProxyCommand, which\n\
         # refuses the connection and says why. Loopback and the container bridge stay direct (the\n\
         # agent checks the address). A ProxyCommand or ProxyJump in ~/.ssh/config comes first and\n\
         # wins.\n\
         Match host \"*,!localhost,!::1\"\n    \
         ProxyCommand {command} %h %p %n\n"
    );
    if let Some(socket) = &settings.ssh_agent {
        let socket = socket.as_str();
        // ssh expands `%` tokens and `${VAR}` in IdentityAgent, and the value is one quoted word.
        if socket.contains(['%', '$', '"']) || socket.chars().any(char::is_control) {
            return Err(ConfigError::GuestPath {
                reason: format!("the SSH agent socket {socket:?} can't be an IdentityAgent"),
            });
        }
        // The socket asks the user's agent on the host, for the hosts the workspace has it on
        // for. It holds no key in the guest.
        let _ = writeln!(config, "    IdentityAgent \"{socket}\"");
    }
    Ok(config)
}

/// The Docker CLI passes these to every container and build it starts. Containers reach the
/// guest's proxy at the bridge gateway, not at loopback; the bypass list is the guest's.
///
/// The file is the user's (`docker login` keeps `auths` and `credHelpers` in it), so Puddle owns
/// only `proxies.default` and merges it in; other daemons' `proxies` entries stay too.
fn docker_cli_config(container: SocketAddr, no_proxy: &str) -> Result<MergeSpec, ConfigError> {
    let proxy = url(container);
    let default = serde_json::json!({
        "httpProxy": proxy,
        "httpsProxy": proxy,
        "noProxy": no_proxy,
    });
    MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(&["proxies", "default"], &default)],
    )
    .map_err(|e| path_error(&e))
}

#[cfg(test)]
mod tests;
