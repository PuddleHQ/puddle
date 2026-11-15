// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle-freshness` command: what the scheduled `freshness` workflow runs, and the checks
//! around it.
//!
//! - `refresh` signs the next file over the one on disk (sequence plus one, expiry seven days out)
//!   and replaces the file.
//! - `verify` is the client's check, run on a file with a public key.
//! - `public-key` prints the public half of a seed, the value to compile into the client.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::VerifyingKey;

use crate::{DEFAULT_VALIDITY_SECS, PLACEHOLDER_SEED, Policy, public_key, refresh, verify};

/// What went wrong on the command line or with a file.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Bad command line; the text names the problem and the usage follows.
    #[error("{0}\n\n{usage}", usage = USAGE)]
    Usage(String),
    /// A file could not be read or written.
    #[error("{context}: {source}")]
    Io {
        /// What was being done, with the path.
        context: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A key or seed is not 32 bytes of base64 (or not a curve point).
    #[error("cannot read {what}: {reason}")]
    Key {
        /// Which value.
        what: String,
        /// Why not.
        reason: String,
    },
    /// The freshness file was refused, or the next one could not be made.
    #[error("freshness: {0}")]
    Freshness(#[from] crate::FreshnessError),
}

type Result<T> = std::result::Result<T, CliError>;

/// Command-line help.
pub const USAGE: &str = "\
usage: puddle-freshness <refresh|verify|public-key> [options]

  refresh     sign the next freshness file over the one at --file (sequence plus one)
  verify      check --file as a client would, against --key
  public-key  print the base64 public key of the seed

  --file <path>       the file (default: freshness.json)
  --seed-env <NAME>   refresh, public-key: base64 32-byte seed in this variable
  --placeholder-key   refresh, public-key: the public test seed; no client trusts it
  --key <base64>      verify: the public key
  --days <n>          refresh: days the file stays valid (default 7)
  --now <unix secs>   the clock (default: the system clock)";

fn key_error(what: &str, reason: &dyn std::fmt::Display) -> CliError {
    CliError::Key {
        what: what.to_owned(),
        reason: reason.to_string(),
    }
}

fn io_error(context: String, source: std::io::Error) -> CliError {
    CliError::Io { context, source }
}

/// Parsed options: `--flag value` pairs and the one switch.
struct Options {
    values: Vec<(String, String)>,
    placeholder: bool,
}

impl Options {
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut values = Vec::new();
        let mut placeholder = false;
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            if arg == "--placeholder-key" {
                placeholder = true;
            } else if ["--file", "--seed-env", "--key", "--days", "--now"].contains(&arg.as_str()) {
                let v = it
                    .next()
                    .ok_or_else(|| CliError::Usage(format!("{arg} needs a value")))?;
                values.push((arg, v));
            } else {
                return Err(CliError::Usage(format!("unknown argument {arg}")));
            }
        }
        Ok(Self {
            values,
            placeholder,
        })
    }

    fn value(&self, flag: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(f, _)| f == flag)
            .map(|(_, v)| v.as_str())
    }

    fn number(&self, flag: &str) -> Result<Option<u64>> {
        self.value(flag)
            .map(|text| {
                text.parse::<u64>()
                    .map_err(|e| CliError::Usage(format!("{flag}: {e}")))
            })
            .transpose()
    }

    /// The signing seed: the public test seed, or the base64 seed in the variable `--seed-env` names.
    fn seed(&self) -> Result<[u8; 32]> {
        if self.placeholder {
            return Ok(PLACEHOLDER_SEED);
        }
        let name = self
            .value("--seed-env")
            .ok_or_else(|| CliError::Usage("pass --seed-env <NAME> or --placeholder-key".into()))?;
        let value = std::env::var(name)
            .map_err(|_| CliError::Usage(format!("{name} is not set (or not UTF-8)")))?;
        parse_seed(&value)
    }
}

/// Runs the command in `args` (without the program name) and returns the line to print.
///
/// # Errors
///
/// [`CliError::Usage`] for a bad command line, or the command's error.
pub fn run(args: impl IntoIterator<Item = OsString>) -> Result<String> {
    let mut args: Vec<String> = args
        .into_iter()
        .map(|a| {
            a.into_string()
                .map_err(|a| CliError::Usage(format!("argument is not UTF-8: {}", a.display())))
        })
        .collect::<Result<_>>()?;
    if args.is_empty() {
        return Err(CliError::Usage("no command".into()));
    }
    let action = args.remove(0);
    let opts = Options::parse(args)?;
    let file = PathBuf::from(opts.value("--file").unwrap_or("freshness.json"));
    let now = match opts.number("--now")? {
        Some(now) => now,
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    match action.as_str() {
        "refresh" => {
            let validity = opts
                .number("--days")?
                .map_or(DEFAULT_VALIDITY_SECS, |days| days.saturating_mul(86_400));
            refresh_file(&file, &opts.seed()?, now, validity)
        }
        "public-key" => Ok(public_key_text(&opts.seed()?)),
        "verify" => {
            let key = opts
                .value("--key")
                .ok_or_else(|| CliError::Usage("verify needs --key <base64>".into()))?;
            verify_file(&file, key, now)
        }
        other => Err(CliError::Usage(format!("unknown command {other}"))),
    }
}

/// The project name the file vouches for.
pub const PROJECT: &str = "puddle";

/// The longest window the client accepts: twice the refresh window, so a missed refresh is not
/// an outage but a key that signs "valid for a year" is refused.
pub const MAX_VALIDITY_SECS: u64 = 2 * DEFAULT_VALIDITY_SECS;

/// A highest sequence no honest daily refresh reaches (about 2.7 million years).
pub const SEQUENCE_CEILING: u64 = 1_000_000_000;

/// Decodes a base64 32-byte value (a seed or a public key).
fn decode32(what: &str, text: &str) -> Result<[u8; 32]> {
    let raw = STANDARD
        .decode(text.trim())
        .map_err(|e| key_error(what, &e))?;
    <[u8; 32]>::try_from(raw.as_slice())
        .map_err(|_| key_error(what, &format!("{} bytes, not 32", raw.len())))
}

/// The seed in a base64 text.
///
/// # Errors
///
/// [`CliError::Key`] when it is not 32 bytes of base64.
pub fn parse_seed(text: &str) -> Result<[u8; 32]> {
    decode32("the signing key seed", text)
}

/// Writes the next freshness file to `path`.
///
/// # Errors
///
/// [`CliError::Io`] for the file, [`CliError::Freshness`] when the file on disk cannot be
/// advanced (unreadable, or signed by another key).
pub fn refresh_file(path: &Path, seed: &[u8; 32], now: u64, validity_secs: u64) -> Result<String> {
    let previous = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(format!("reading {}", path.display()), e)),
    };
    let next = refresh(previous.as_deref(), seed, PROJECT, now, validity_secs)?;
    std::fs::write(path, &next.bytes)
        .map_err(|e| io_error(format!("writing {}", path.display()), e))?;
    Ok(format!(
        "freshness {}: sequence {}, expires {} ({} s from {})",
        path.display(),
        next.statement.sequence,
        next.statement.expires_at,
        validity_secs,
        now
    ))
}

/// Checks the file at `path` as a client would, against the public key `key` (base64).
///
/// # Errors
///
/// [`CliError::Freshness`] for any refusal, [`CliError::Key`] for a bad key.
pub fn verify_file(path: &Path, key: &str, now: u64) -> Result<String> {
    let key = VerifyingKey::from_bytes(&decode32("the public key", key)?)
        .map_err(|e| key_error("the public key", &e))?;
    let bytes =
        std::fs::read(path).map_err(|e| io_error(format!("reading {}", path.display()), e))?;
    let policy = Policy {
        project: PROJECT,
        max_validity_secs: MAX_VALIDITY_SECS,
        sequence_ceiling: SEQUENCE_CEILING,
    };
    let statement = verify(&bytes, &[key], &policy, now, None)?;
    Ok(format!(
        "freshness ok: sequence {}, {} s left",
        statement.sequence,
        statement.expires_at - now
    ))
}

/// The base64 public key of `seed`.
#[must_use]
pub fn public_key_text(seed: &[u8; 32]) -> String {
    STANDARD.encode(public_key(seed).to_bytes())
}

#[cfg(test)]
mod tests;
