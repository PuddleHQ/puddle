// SPDX-License-Identifier: AGPL-3.0-or-later
//! Schema versions and the migration runner.
//!
//! A document's `schema_version` names the shape it was written in. Version 1 is the first
//! shape. To change a shape:
//!
//! 1. bump the kind's `*_SCHEMA_VERSION`;
//! 2. append a [`Migration`] with `from` = the old version to the kind's list (`GLOBAL` or
//!    `WORKSPACE` below); it rewrites the JSON object in place (rename a key, convert a value);
//! 3. add a test that loads a stored document of the old version and checks the result.
//!
//! The lists are checked by a unit test: one step per version, in order, none missing.

use serde_json::{Map, Value};

use crate::SettingsError;

/// One step: rewrites a document of version `from` into version `from + 1`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Migration {
    pub(crate) from: u32,
    pub(crate) apply: fn(&mut Map<String, Value>) -> Result<(), String>,
}

/// Steps for [`crate::GlobalSettings`], oldest first.
pub(crate) const GLOBAL: &[Migration] = &[Migration {
    from: 1,
    apply: workspace_defaults,
}];
/// Steps for [`crate::WorkspaceSettings`], oldest first.
pub(crate) const WORKSPACE: &[Migration] = &[];

/// Version 1 called the defaults every workspace gets `sandbox_defaults`. A document that somehow
/// has both keeps the new one, and the old stays as an unknown field.
#[expect(
    clippy::unnecessary_wraps,
    reason = "has the signature of Migration::apply"
)]
fn workspace_defaults(m: &mut Map<String, Value>) -> Result<(), String> {
    if !m.contains_key("workspace_defaults")
        && let Some(v) = m.remove("sandbox_defaults")
    {
        m.insert("workspace_defaults".to_owned(), v);
    }
    Ok(())
}

/// The key every document carries its version under.
pub(crate) const VERSION_KEY: &str = "schema_version";

/// Takes `doc` apart into its version and the remaining object, and migrates that object up to
/// `current`. Returns the migrated object and the version it was read in.
pub(crate) fn upgrade(
    kind: &'static str,
    doc: Value,
    current: u32,
    steps: &[Migration],
) -> Result<(Map<String, Value>, u32), SettingsError> {
    let Value::Object(mut map) = doc else {
        return Err(SettingsError::NotAnObject { kind });
    };
    let found = match map.remove(VERSION_KEY) {
        None => 1,
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| n >= 1)
            .ok_or_else(|| SettingsError::InvalidVersion {
                kind,
                found: truncate(&v.to_string()),
            })?,
    };
    if found > current {
        return Err(SettingsError::NewerSchema {
            kind,
            found,
            supported: current,
        });
    }
    let mut version = found;
    while version < current {
        let step = steps
            .iter()
            .find(|m| m.from == version)
            .ok_or(SettingsError::Migration {
                kind,
                from: version,
                reason: "no migration for this version".to_owned(),
            })?;
        (step.apply)(&mut map).map_err(|reason| SettingsError::Migration {
            kind,
            from: version,
            reason,
        })?;
        version += 1;
    }
    Ok((map, found))
}

fn truncate(s: &str) -> String {
    const MAX: usize = 32;
    let mut out: String = s.chars().take(MAX).collect();
    if s.chars().count() > MAX {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests;
