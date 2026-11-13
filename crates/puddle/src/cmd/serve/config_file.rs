// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle serve` config file: one TOML file of start-up options, read strictly.
//!
//! An unknown key, a value of the wrong type or an invalid value, a repeated key and a syntax
//! error each refuse to start, and the message names the file, the line and what to write. A
//! relative path inside the file means the folder that holds the file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::de::{DeTable, DeValue};

use super::options::{Kind, OPTIONS, Value, spec_for_key};

/// A start-up option that cannot be used. The text is the whole message, without the program
/// name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// An environment variable holds a value the option does not accept.
    #[error("{name} has the invalid value {value:?}: expected {why}")]
    Environment {
        /// The variable.
        name: &'static str,
        /// What it holds.
        value: String,
        /// What was expected.
        why: String,
    },
    /// The options set remote serving in a way that does not fit together.
    #[error("remote serving: {0}")]
    Remote(String),
    /// The working directory is unknown, so a relative path cannot be made absolute.
    #[error("cannot find the working directory: {0}")]
    WorkingDirectory(String),
    /// The config file cannot be read.
    #[error(
        "cannot read the config file {}: {reason}; check the path given for the config file",
        .file.display()
    )]
    Unreadable {
        /// The file.
        file: PathBuf,
        /// The operating system's reason.
        reason: String,
    },
    /// The config file is not valid TOML (this includes a key that appears twice).
    #[error("{}:{line}:{column}: {message}; {fix}", .file.display())]
    Syntax {
        /// The file.
        file: PathBuf,
        /// The line, from 1.
        line: usize,
        /// The column, from 1.
        column: usize,
        /// What the TOML parser says.
        message: String,
        /// What to do.
        fix: &'static str,
    },
    /// A key that is not an option.
    #[error(
        "{}:{line}: unknown key `{key}`; the keys are {keys}; fix the spelling or remove the line",
        .file.display()
    )]
    UnknownKey {
        /// The file.
        file: PathBuf,
        /// The line, from 1.
        line: usize,
        /// The key.
        key: String,
        /// The keys the file may hold.
        keys: String,
    },
    /// A key with a value of the wrong type or an invalid value.
    #[error("{}:{line}: `{key}` must be {expected}, found {found}; write it as `{key} = {example}`", .file.display())]
    BadValue {
        /// The file.
        file: PathBuf,
        /// The line, from 1.
        line: usize,
        /// The key.
        key: &'static str,
        /// What the key takes.
        expected: &'static str,
        /// What the file has.
        found: String,
        /// A valid line.
        example: &'static str,
    },
}

/// One key of the file, checked.
type Entry = Result<(&'static str, Value), ConfigError>;

/// The values a config file sets, by option key; paths are absolute.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileValues {
    values: BTreeMap<&'static str, Value>,
}

impl FileValues {
    /// The value the file gave an option.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

/// Reads and checks the config file at `path`, which is absolute.
///
/// # Errors
///
/// [`ConfigError`] when the file cannot be read or is not valid.
pub fn load(path: &Path) -> Result<FileValues, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Unreadable {
        file: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    parse(&text, path)
}

/// Checks the text of the config file at `path`. A relative path in it means `path`'s folder.
///
/// # Errors
///
/// [`ConfigError`] for the first problem, in file order.
pub fn parse(text: &str, path: &Path) -> Result<FileValues, ConfigError> {
    let table = DeTable::parse(text).map_err(|e| {
        let (line, column) = e.span().map_or((1, 1), |span| position(text, span.start));
        let message = e.message();
        let fix = if message.contains("duplicate key") {
            "a key may appear once: remove one of the lines"
        } else {
            "fix the TOML on that line"
        };
        ConfigError::Syntax {
            file: path.to_path_buf(),
            line,
            column,
            message: message.to_owned(),
            fix,
        }
    })?;
    let folder = path.parent().unwrap_or(Path::new(""));
    let mut found: Vec<(usize, Entry)> = Vec::new();
    for (key, value) in table.get_ref() {
        let line = position(text, key.span().start).0;
        let entry = match spec_for_key(key.get_ref()).filter(|spec| spec.in_file) {
            None => Err(ConfigError::UnknownKey {
                file: path.to_path_buf(),
                line,
                key: key.get_ref().to_string(),
                keys: file_keys(),
            }),
            Some(spec) => value_of(value.get_ref(), spec.kind, folder)
                .map(|value| (spec.key, value))
                .map_err(|found| ConfigError::BadValue {
                    file: path.to_path_buf(),
                    line,
                    key: spec.key,
                    expected: spec.kind.expects(),
                    found,
                    example: spec.example,
                }),
        };
        found.push((line, entry));
    }
    found.sort_by_key(|(line, _)| *line);
    let mut values = BTreeMap::new();
    for (_, entry) in found {
        let (key, value) = entry?;
        values.insert(key, value);
    }
    Ok(FileValues { values })
}

/// The value of a TOML entry as the option's kind, or a description of what the file has.
fn value_of(value: &DeValue<'_>, kind: Kind, folder: &Path) -> Result<Value, String> {
    match (kind, value) {
        (Kind::Port, DeValue::Integer(int)) => i64::from_str_radix(int.as_str(), int.radix())
            .ok()
            .and_then(|n| u16::try_from(n).ok())
            .map(Value::Port)
            .ok_or_else(|| format!("the integer {}", int.as_str())),
        (Kind::Path, DeValue::String(text)) => kind
            .parse_text(text)
            .map(|value| match value {
                Value::Path(path) if path.is_relative() => Value::Path(folder.join(path)),
                other => other,
            })
            .map_err(|_| "an empty string".to_owned()),
        (Kind::Log | Kind::Ip | Kind::Domain, DeValue::String(text)) => kind
            .parse_text(text)
            .map_err(|_| format!("the string {:?}", text.as_ref())),
        (Kind::Bool, DeValue::Boolean(on)) => Ok(Value::Bool(*on)),
        (kind, DeValue::Array(items)) if kind.is_list() => {
            let mut texts = Vec::new();
            for item in items {
                match item.get_ref() {
                    DeValue::String(text) if !text.contains(',') => texts.push(text.as_ref()),
                    DeValue::String(text) => return Err(format!("the item {:?}", text.as_ref())),
                    other => return Err(format!("an array with {}", describe(other))),
                }
            }
            kind.parse_items(&texts)
                .map_err(|_| format!("the array {texts:?}"))
        }
        (_, other) => Err(describe(other)),
    }
}

/// A value's type with its article: "an integer", "a string".
fn describe(value: &DeValue<'_>) -> String {
    let name = value.type_str();
    let article = if name.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {name}")
}

/// The keys a file may hold, for the unknown-key message.
fn file_keys() -> String {
    let keys: Vec<&str> = OPTIONS
        .iter()
        .filter(|spec| spec.in_file)
        .map(|spec| spec.key)
        .collect();
    keys.join(", ")
}

/// The line and column, both from 1, of a byte offset in `text`.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |last| last.chars().count())
        + 1;
    (line, column)
}

#[cfg(test)]
mod tests;
