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
use std::path::{Path, PathBuf};

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
}

/// Every option of `puddle serve`, in the order `--print-config` lists them.
pub const OPTIONS: [Spec; 5] = [
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

/// A value an option was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A port.
    Port(u16),
    /// A path.
    Path(PathBuf),
    /// A log filter, as written.
    Log(String),
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Port(port) => write!(f, "{port}"),
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Log(text) => f.write_str(text),
        }
    }
}

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A command-line flag.
    Flag(&'static str),
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
    let mut values = BTreeMap::new();
    let mut file: Option<(PathBuf, config_file::FileValues)> = None;
    for spec in &OPTIONS {
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
        return Ok(Some((
            absolute(value.clone(), cwd),
            Source::Flag(spec.flag()),
        )));
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
