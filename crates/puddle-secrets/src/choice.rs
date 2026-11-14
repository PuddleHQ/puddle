// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which store keeps an install's secrets: decided once, written to `<data>/secrets/store.json`
//! and never decided again.
//!
//! The rules are a pure function ([`decide`]) of what the start knows, so a table of cases covers
//! them. Nothing here moves a value or switches a recorded store: a start that asks for the other
//! store is refused ([`ChoiceError::Mismatch`]).

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The `secret_store` option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StoreOption {
    /// Pick by the rules of [`decide`].
    #[default]
    Auto,
    /// The operating system's credential store.
    Os,
    /// Puddle's own encrypted file vault.
    File,
}

impl StoreOption {
    /// The values a person may write.
    pub const VALUES: &'static str = "auto, os or file";

    /// The option written as text; `None` for anything but `auto`, `os` and `file`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "os" => Some(Self::Os),
            "file" => Some(Self::File),
            _ => None,
        }
    }

    /// The option as written.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Os => "os",
            Self::File => "file",
        }
    }

    fn kind(self) -> Option<StoreKind> {
        match self {
            Self::Auto => None,
            Self::Os => Some(StoreKind::Os),
            Self::File => Some(StoreKind::File),
        }
    }
}

/// A store an install can keep its secrets in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreKind {
    /// The operating system's credential store.
    Os,
    /// Puddle's own encrypted file vault.
    File,
}

impl StoreKind {
    /// The value of the `secret_store` option that names it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Os => "os",
            Self::File => "file",
        }
    }

    /// What a person calls it.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Os => "the operating system's credential store",
            Self::File => "Puddle's file vault",
        }
    }
}

/// What a start knows when it picks a store.
#[derive(Debug, Clone, Copy)]
pub struct Inputs {
    /// The `secret_store` option.
    pub option: StoreOption,
    /// The store this data folder recorded, when it has.
    pub recorded: Option<StoreKind>,
    /// Whether the folder's rows name a secret (a token, an environment secret).
    pub names_secret: bool,
    /// Whether this is Linux.
    pub linux: bool,
    /// Whether the start has the headless flag.
    pub headless: bool,
}

/// The rule that fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// The data folder had decided already.
    Recorded,
    /// The `secret_store` option named a store.
    Option,
    /// Rows name secrets, so the values are in the operating system's store.
    ExistingSecrets,
    /// Windows and macOS.
    OtherPlatform,
    /// Linux with the headless flag.
    Headless,
    /// Linux without it.
    LinuxDefault,
}

impl Rule {
    /// The words written to `store.json`.
    #[must_use]
    pub fn why(self) -> &'static str {
        match self {
            Self::Recorded => "decided by an earlier start",
            Self::Option => "the secret_store option named it",
            Self::ExistingSecrets => {
                "rows of this install name secrets, which an earlier version kept in the operating system's store"
            }
            Self::OtherPlatform => "Windows and macOS use the operating system's store",
            Self::Headless => "Linux, started with the headless flag",
            Self::LinuxDefault => "Linux, started without the headless flag",
        }
    }
}

/// When the decision is written to `store.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recording {
    /// It is written already.
    Already,
    /// At this start.
    Now,
    /// Once the operating system's store has taken a test write. A Linux start without the
    /// headless flag whose keyring does not work has stored nothing, so it decides nothing: the
    /// next start, with the flag, may still pick the vault.
    WhenWorking,
}

/// The decision of a start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The store to use.
    pub store: StoreKind,
    /// Why.
    pub rule: Rule,
    /// When to write it down.
    pub recording: Recording,
}

/// Why no store could be chosen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChoiceError {
    /// The option names the other store than the data folder recorded.
    #[error(
        "this data folder keeps its secrets in {}, and `secret_store` asks for {}. Puddle never moves secrets between stores on its own: set `secret_store` to `auto` or `{}`",
        .recorded.describe(),
        .asked.describe(),
        .recorded.as_str()
    )]
    Mismatch {
        /// What the folder recorded.
        recorded: StoreKind,
        /// What the option asked for.
        asked: StoreKind,
    },
}

/// Picks the store: the data folder's record first, then the rules of the first start.
///
/// # Errors
/// [`ChoiceError::Mismatch`] when the option names the other store than the one recorded.
pub fn decide(inputs: Inputs) -> Result<Decision, ChoiceError> {
    let decision = |store, rule, recording| Decision {
        store,
        rule,
        recording,
    };
    if let Some(recorded) = inputs.recorded {
        return match inputs.option.kind() {
            Some(asked) if asked != recorded => Err(ChoiceError::Mismatch { recorded, asked }),
            _ => Ok(decision(recorded, Rule::Recorded, Recording::Already)),
        };
    }
    Ok(if let Some(asked) = inputs.option.kind() {
        decision(asked, Rule::Option, Recording::Now)
    } else if inputs.names_secret {
        decision(StoreKind::Os, Rule::ExistingSecrets, Recording::Now)
    } else if !inputs.linux {
        decision(StoreKind::Os, Rule::OtherPlatform, Recording::Now)
    } else if inputs.headless {
        decision(StoreKind::File, Rule::Headless, Recording::Now)
    } else {
        decision(StoreKind::Os, Rule::LinuxDefault, Recording::WhenWorking)
    })
}

const FOLDER: &str = "secrets";
const FILE: &str = "store.json";
const VERSION: u8 = 1;

/// What `store.json` holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreRecord {
    /// The format version, 1.
    pub version: u8,
    /// The store.
    pub store: StoreKind,
    /// When it was decided, UTC.
    pub decided: String,
    /// The rule that fired.
    pub why: String,
}

/// Why `store.json` could not be read or written. The data folder's path is in neither text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The file system refused.
    #[error("`secrets/store.json` could not be read or written")]
    Io,
    /// The file is not a store record of a version this program knows. Puddle will not guess
    /// which store holds the secrets.
    #[error(
        "`secrets/store.json` is not a store record this version of Puddle understands; it says where this data folder keeps its secrets, so Puddle does not choose again. Restore the file or ask for help before deleting it"
    )]
    Unreadable,
}

/// The folder that holds the vault, the record and nothing else.
#[must_use]
pub fn secrets_folder(data: &Path) -> PathBuf {
    data.join(FOLDER)
}

/// Where the file vault lives.
#[must_use]
pub fn vault_path(data: &Path) -> PathBuf {
    secrets_folder(data).join("vault")
}

/// The store this data folder recorded; `None` before the first start decided.
///
/// # Errors
/// [`RecordError`] for a file that cannot be read or is not a record.
pub fn read_record(data: &Path) -> Result<Option<StoreRecord>, RecordError> {
    let text = match std::fs::read_to_string(secrets_folder(data).join(FILE)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RecordError::Io),
    };
    match serde_json::from_str::<StoreRecord>(&text) {
        Ok(record) if record.version == VERSION => Ok(Some(record)),
        _ => Err(RecordError::Unreadable),
    }
}

/// Writes the record, atomically, in an owner-only folder.
///
/// # Errors
/// [`RecordError::Io`] when the file system refuses.
pub fn write_record(data: &Path, store: StoreKind, rule: Rule) -> Result<(), RecordError> {
    let folder = secrets_folder(data);
    puddle_fs::private::create_dir(&folder).map_err(|_| RecordError::Io)?;
    let record = StoreRecord {
        version: VERSION,
        store,
        decided: utc(SystemTime::now()),
        why: rule.why().to_owned(),
    };
    let mut body = serde_json::to_vec_pretty(&record).map_err(|_| RecordError::Io)?;
    body.push(b'\n');
    puddle_fs::private::write_atomic(&folder.join(FILE), &body).map_err(|_| RecordError::Io)
}

/// `time` as `2026-11-14T05:33:55Z`.
fn utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let shifted = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

impl fmt::Display for StoreKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.describe())
    }
}

#[cfg(test)]
mod tests;
