// SPDX-License-Identifier: GPL-3.0-or-later
//! The start-up options of `puddle serve`, declared once.
//!
//! [`OPTIONS`] is the only place that names an option: the command-line flag, the environment
//! variable and the config-file key all come from its row. A new option is one more row (and the
//! line that reads it into the host's configuration); the parser, the precedence rules, the
//! config-file check, `--print-config` and the usage test pick it up from the table.
//!
//! Precedence, highest first: the flag, the environment variable, the config file, the default.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use puddle_api::{NetworkEndpoint, PublicHost, RemoteServing};
use puddle_host::RemoteSettings;
use puddle_types::DomainName;

use puddle_log::ScopedFilter;

use super::config_file::{self, ConfigError};

/// What an option's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A TCP port, 0 to 65535.
    Port,
    /// A file or folder path. A relative one means the folder of the config file when it comes
    /// from there, and the working directory otherwise.
    Path,
    /// A log filter, `PUDDLE_LOG` syntax.
    Log,
    /// `true` or `false`. On the command line the option is a flag without a value:
    /// `--name` for true and `--no-name` for false.
    Bool,
    /// One IP address.
    Ip,
    /// A comma-separated list of IP addresses (an array of strings in the file).
    IpList,
    /// A comma-separated list of public hosts, each a host name with an optional port (an array
    /// of strings in the file).
    HostList,
    /// A domain name.
    Domain,
}

impl Kind {
    /// What a person may write, for an error message.
    #[must_use]
    pub const fn expects(self) -> &'static str {
        match self {
            Self::Port => "a port number from 0 to 65535",
            Self::Path => "a non-empty path",
            Self::Log => {
                "error, warn, info, debug or trace, optionally followed by crate=level entries"
            }
            Self::Bool => "true or false",
            Self::Ip => "an IP address",
            Self::IpList => "a comma-separated list of IP addresses",
            Self::HostList => {
                "a comma-separated list of host names, each with an optional port, such as puddle.example.org or puddle.example.org:8443"
            }
            Self::Domain => "a domain name such as ws.example.org",
        }
    }

    /// Whether the value is a list, written as an array in the file.
    #[must_use]
    pub const fn is_list(self) -> bool {
        matches!(self, Self::IpList | Self::HostList)
    }

    /// Whether the option is a flag without a value (`--name`, `--no-name`).
    #[must_use]
    pub const fn is_switch(self) -> bool {
        matches!(self, Self::Bool)
    }

    /// The value made of separate items (the entries of a file's array).
    ///
    /// # Errors
    ///
    /// What was expected, when an item is not valid or the list is empty.
    pub fn parse_items(self, items: &[&str]) -> Result<Value, &'static str> {
        let bad = self.expects();
        if items.is_empty() {
            return Err(bad);
        }
        match self {
            Self::IpList => items
                .iter()
                .map(|item| item.trim().parse::<IpAddr>().map_err(|_| bad))
                .collect::<Result<_, _>>()
                .map(Value::IpList),
            Self::HostList => items
                .iter()
                .map(|item| {
                    PublicHost::parse(item.trim())
                        .map(|host| host.authority())
                        .map_err(|_| bad)
                })
                .collect::<Result<_, _>>()
                .map(Value::Hosts),
            _ => Err(bad),
        }
    }

    /// The value written as text (a flag, an environment variable).
    ///
    /// # Errors
    ///
    /// What was expected, when `text` is not a value of this kind.
    pub fn parse_text(self, text: &str) -> Result<Value, &'static str> {
        match self {
            Self::Port => text.parse().map(Value::Port).map_err(|_| self.expects()),
            Self::Path if text.is_empty() => Err(self.expects()),
            Self::Path => Ok(Value::Path(PathBuf::from(text))),
            Self::Log => ScopedFilter::parse(text)
                .map(|_| Value::Log(text.to_owned()))
                .map_err(|_| self.expects()),
            Self::Bool => match text {
                "true" => Ok(Value::Bool(true)),
                "false" => Ok(Value::Bool(false)),
                _ => Err(self.expects()),
            },
            Self::Ip => text.parse().map(Value::Ip).map_err(|_| self.expects()),
            Self::IpList | Self::HostList => {
                let items: Vec<&str> = text.split(',').collect();
                self.parse_items(&items)
            }
            Self::Domain => {
                let name = text.to_ascii_lowercase();
                let name = name.strip_suffix('.').unwrap_or(&name);
                DomainName::parse_normalised(name)
                    .map(|name| Value::Domain(name.as_str().to_owned()))
                    .map_err(|_| self.expects())
            }
        }
    }
}

/// One option of `puddle serve`.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// The name in the config file and in `--print-config`.
    pub key: &'static str,
    /// The flag with its value placeholder: `--port <n>`.
    pub usage: &'static str,
    /// The environment variable.
    pub env: &'static str,
    /// What the value is.
    pub kind: Kind,
    /// Whether the config file may set it (the file's own location may not).
    pub in_file: bool,
    /// A valid value written as TOML, for the hint of a config-file error.
    pub example: &'static str,
    /// What the option does when nothing sets it.
    pub default: &'static str,
}

impl Spec {
    /// The flag alone: `--port`.
    #[must_use]
    pub fn flag(&self) -> &'static str {
        self.usage.split(' ').next().unwrap_or(self.usage)
    }

    /// The flag that sets a boolean option to false: `--no-remote`.
    #[must_use]
    pub fn negated_flag(&self) -> Option<String> {
        self.kind
            .is_switch()
            .then(|| format!("--no-{}", &self.flag()[2..]))
    }
}

/// Every option of `puddle serve`, in the order `--print-config` lists them.
pub const OPTIONS: [Spec; 12] = [
    Spec {
        key: "config",
        usage: "--config <path>",
        env: "PUDDLE_CONFIG",
        kind: Kind::Path,
        in_file: false,
        example: "\"/etc/puddle/serve.toml\"",
        default: "no config file",
    },
    Spec {
        key: "port",
        usage: "--port <n>",
        env: "PUDDLE_PORT",
        kind: Kind::Port,
        in_file: true,
        example: "8080",
        default: "a free port",
    },
    Spec {
        key: "connection_file",
        usage: "--connection-file <path>",
        env: "PUDDLE_CONNECTION_FILE",
        kind: Kind::Path,
        in_file: true,
        example: "\"api.json\"",
        default: "api.json in the data folder",
    },
    Spec {
        key: "data_dir",
        usage: "--data-dir <path>",
        env: "PUDDLE_DATA_DIR",
        kind: Kind::Path,
        in_file: true,
        example: "\"/srv/puddle\"",
        default: "the per-user data folder",
    },
    Spec {
        key: "log",
        usage: "--log <filter>",
        env: "PUDDLE_LOG",
        kind: Kind::Log,
        in_file: true,
        example: "\"info,hyper=debug\"",
        default: "info",
    },
    Spec {
        key: "remote",
        usage: "--remote",
        env: "PUDDLE_REMOTE",
        kind: Kind::Bool,
        in_file: true,
        example: "true",
        default: "off",
    },
    Spec {
        key: "public_hosts",
        usage: "--public-hosts <host,...>",
        env: "PUDDLE_PUBLIC_HOSTS",
        kind: Kind::HostList,
        in_file: true,
        example: "[\"puddle.example.org\"]",
        default: "none",
    },
    Spec {
        key: "workspace_base_domain",
        usage: "--workspace-base-domain <domain>",
        env: "PUDDLE_WORKSPACE_BASE_DOMAIN",
        kind: Kind::Domain,
        in_file: true,
        example: "\"ws.example.net\"",
        default: "subdomains of the first public host",
    },
    Spec {
        key: "listen_interface",
        usage: "--listen-interface <ip>",
        env: "PUDDLE_LISTEN_INTERFACE",
        kind: Kind::Ip,
        in_file: true,
        example: "\"192.168.1.10\"",
        default: "no network listener",
    },
    Spec {
        key: "listen_port",
        usage: "--listen-port <n>",
        env: "PUDDLE_LISTEN_PORT",
        kind: Kind::Port,
        in_file: true,
        example: "8443",
        default: "8443",
    },
    Spec {
        key: "allowed_proxies",
        usage: "--allowed-proxies <ip,...>",
        env: "PUDDLE_ALLOWED_PROXIES",
        kind: Kind::IpList,
        in_file: true,
        example: "[\"192.168.1.20\"]",
        default: "none",
    },
    Spec {
        key: "multi_user",
        usage: "--multi-user",
        env: "PUDDLE_MULTI_USER",
        kind: Kind::Bool,
        in_file: true,
        example: "true",
        default: "off",
    },
];

/// The row of the table with this key.
#[must_use]
pub fn spec_for_key(key: &str) -> Option<&'static Spec> {
    OPTIONS.iter().find(|spec| spec.key == key)
}

/// The row of the table with this flag.
#[must_use]
pub fn spec_for_flag(flag: &str) -> Option<&'static Spec> {
    OPTIONS.iter().find(|spec| spec.flag() == flag)
}

/// The boolean row whose `--no-name` flag this is.
#[must_use]
pub fn spec_for_negated_flag(flag: &str) -> Option<&'static Spec> {
    OPTIONS
        .iter()
        .find(|spec| spec.negated_flag().as_deref() == Some(flag))
}

/// A value an option was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A port.
    Port(u16),
    /// A path.
    Path(PathBuf),
    /// A log filter, as written.
    Log(String),
    /// A switch.
    Bool(bool),
    /// An IP address.
    Ip(IpAddr),
    /// IP addresses.
    IpList(Vec<IpAddr>),
    /// Public hosts, each as its authority (lower case, `:443` dropped).
    Hosts(Vec<String>),
    /// A domain name, lower case.
    Domain(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Port(port) => write!(f, "{port}"),
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Log(text) | Self::Domain(text) => f.write_str(text),
            Self::Bool(on) => write!(f, "{on}"),
            Self::Ip(ip) => write!(f, "{ip}"),
            Self::IpList(ips) => {
                let ips: Vec<String> = ips.iter().map(ToString::to_string).collect();
                f.write_str(&ips.join(","))
            }
            Self::Hosts(hosts) => f.write_str(&hosts.join(",")),
        }
    }
}

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A command-line flag.
    Flag(&'static str),
    /// The `--no-name` flag of a switch; holds the switch's own flag, `--name`.
    NegatedFlag(&'static str),
    /// An environment variable.
    Environment(&'static str),
    /// The config file.
    File(PathBuf),
    /// Nothing set it.
    Default,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flag(flag) => write!(f, "flag {flag}"),
            Self::NegatedFlag(flag) => write!(f, "flag --no-{}", flag.trim_start_matches("--")),
            Self::Environment(name) => write!(f, "environment {name}"),
            Self::File(path) => write!(f, "config file {}", path.display()),
            Self::Default => f.write_str("default"),
        }
    }
}

/// The effective options: for each row of [`OPTIONS`] its value and where it came from. An option
/// nothing sets has no value; its default is what the host does without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effective {
    values: BTreeMap<&'static str, (Value, Source)>,
}

impl Effective {
    /// The value of an option and its source.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&(Value, Source)> {
        self.values.get(key)
    }

    /// The port, when one was set.
    #[must_use]
    pub fn port(&self) -> Option<u16> {
        match self.values.get("port") {
            Some((Value::Port(port), _)) => Some(*port),
            _ => None,
        }
    }

    /// The path an option of kind [`Kind::Path`] was set to, absolute.
    #[must_use]
    pub fn path(&self, key: &str) -> Option<&Path> {
        match self.values.get(key) {
            Some((Value::Path(path), _)) => Some(path),
            _ => None,
        }
    }

    /// The value of a switch, when one was set.
    #[must_use]
    pub fn switch(&self, key: &str) -> Option<bool> {
        match self.values.get(key) {
            Some((Value::Bool(on), _)) => Some(*on),
            _ => None,
        }
    }

    /// Remote serving, from the options that set it: `None` while the switch is off or unset
    /// (the other remote options are then not looked at), the settings otherwise.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Remote`] when the options do not fit together: no public host, a network
    /// listener without a proxy that may connect, or an option that needs another one.
    pub fn remote(&self) -> Result<Option<RemoteSettings>, ConfigError> {
        if self.switch("remote") != Some(true) {
            return Ok(None);
        }
        let fail = |message: String| ConfigError::Remote(message);
        let hosts = match self.values.get("public_hosts") {
            Some((Value::Hosts(hosts), _)) => hosts.as_slice(),
            _ => &[],
        };
        let hosts = hosts
            .iter()
            .map(|text| PublicHost::parse(text).map_err(|e| fail(e.to_string())))
            .collect::<Result<Vec<_>, _>>()?;
        let base = match self.values.get("workspace_base_domain") {
            Some((Value::Domain(domain), _)) => Some(domain.as_str()),
            _ => None,
        };
        let serving = RemoteServing::new(hosts, base).map_err(|e| fail(e.to_string()))?;
        let interface = match self.values.get("listen_interface") {
            Some((Value::Ip(ip), _)) => Some(*ip),
            _ => None,
        };
        let proxies = match self.values.get("allowed_proxies") {
            Some((Value::IpList(ips), _)) => ips.clone(),
            _ => Vec::new(),
        };
        let port = match self.values.get("listen_port") {
            Some((Value::Port(port), _)) => Some(*port),
            _ => None,
        };
        let network = if let Some(interface) = interface {
            Some(
                NetworkEndpoint::new(interface, port.unwrap_or(DEFAULT_LISTEN_PORT), proxies)
                    .map_err(|e| fail(e.to_string()))?,
            )
        } else {
            if port.is_some() || !proxies.is_empty() {
                return Err(fail(
                    "`listen_port` and `allowed_proxies` belong to the network listener; set `listen_interface` too or remove them"
                        .to_owned(),
                ));
            }
            None
        };
        Ok(Some(RemoteSettings { serving, network }))
    }

    /// The log filter, `info` when none was set.
    #[must_use]
    pub fn log_filter(&self) -> ScopedFilter {
        match self.values.get("log") {
            Some((Value::Log(text), _)) => ScopedFilter::parse(text).ok(),
            _ => None,
        }
        .unwrap_or_default()
    }

    /// What `--print-config` prints: one line per option with its value and its source.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for spec in &OPTIONS {
            let line = match self.values.get(spec.key) {
                Some((value, source)) => format!("{} = {value} ({source})\n", spec.key),
                None => format!("{} = {} ({})\n", spec.key, spec.default, Source::Default),
            };
            out.push_str(&line);
        }
        out
    }
}

/// The port of the network listener when `listen_port` is not set.
pub const DEFAULT_LISTEN_PORT: u16 = 8443;

/// The options given as flags, by option key.
pub type Flags = BTreeMap<&'static str, Value>;

/// The key of the option that names the config file.
pub const CONFIG_KEY: &str = "config";

/// Resolves the options: the flags, the environment, the config file, the defaults.
///
/// `env` reads an environment variable; `cwd` is the working directory that a relative path from
/// a flag or the environment means. The table lists `config` first, because the file it names
/// supplies the rows after it.
///
/// # Errors
///
/// [`ConfigError`] for an environment variable with an invalid value, and for a config file that
/// cannot be read or is not valid (the message names the file, the line and the fix).
pub fn resolve(
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<OsString>,
    cwd: &Path,
) -> Result<Effective, ConfigError> {
    resolve_keys(flags, env, cwd, &OPTIONS.map(|spec| spec.key))
}

/// [`resolve`] for the options named in `keys` only, so a bad value of another option (a
/// malformed `PUDDLE_PORT`) does not stop a command that never reads it. The `config` option is
/// always read first, because it names the file the others may come from; the file itself is
/// still checked as a whole.
///
/// # Errors
///
/// [`ConfigError`] as [`resolve`] says, for the options in `keys` and the config file.
pub fn resolve_keys(
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<OsString>,
    cwd: &Path,
    keys: &[&str],
) -> Result<Effective, ConfigError> {
    let mut values = BTreeMap::new();
    let mut file: Option<(PathBuf, config_file::FileValues)> = None;
    for spec in OPTIONS
        .iter()
        .filter(|spec| spec.key == CONFIG_KEY || keys.contains(&spec.key))
    {
        let mut chosen = flag_or_env(spec, flags, env, cwd)?;
        if chosen.is_none() {
            chosen = file.as_ref().and_then(|(path, from_file)| {
                let value = from_file.get(spec.key)?;
                Some((value.clone(), Source::File(path.clone())))
            });
        }
        if let (CONFIG_KEY, Some((Value::Path(path), _))) = (spec.key, &chosen) {
            file = Some((path.clone(), config_file::load(path)?));
        }
        if let Some(chosen) = chosen {
            values.insert(spec.key, chosen);
        }
    }
    Ok(Effective { values })
}

/// The value the flag gave, else the one the environment variable gave.
fn flag_or_env(
    spec: &'static Spec,
    flags: &Flags,
    env: &dyn Fn(&str) -> Option<OsString>,
    cwd: &Path,
) -> Result<Option<(Value, Source)>, ConfigError> {
    if let Some(value) = flags.get(spec.key) {
        let source = match value {
            Value::Bool(false) => Source::NegatedFlag(spec.flag()),
            _ => Source::Flag(spec.flag()),
        };
        return Ok(Some((absolute(value.clone(), cwd), source)));
    }
    let Some(raw) = env(spec.env) else {
        return Ok(None);
    };
    let invalid = |why: &str| ConfigError::Environment {
        name: spec.env,
        value: raw.to_string_lossy().into_owned(),
        why: why.to_owned(),
    };
    let text = raw.to_str().ok_or_else(|| invalid("not valid Unicode"))?;
    let value = spec.kind.parse_text(text).map_err(invalid)?;
    Ok(Some((absolute(value, cwd), Source::Environment(spec.env))))
}

/// A path value made absolute against `cwd`; other values unchanged.
fn absolute(value: Value, cwd: &Path) -> Value {
    match value {
        Value::Path(path) if path.is_relative() => Value::Path(cwd.join(path)),
        other => other,
    }
}

#[cfg(test)]
mod tests;
