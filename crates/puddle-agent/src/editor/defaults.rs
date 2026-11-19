// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor defaults the host planned (`EDITOR_DEFAULTS_GUEST`), merged into the code-server
//! user's own settings before the server starts.
//!
//! The rule is three-way and per key: the planned value (what Puddle wants now), the value Puddle
//! last wrote (`editor-defaults-applied.json` in the data folder) and the value the file holds
//! now. Puddle writes a key only while the file still holds what Puddle wrote last (a key that
//! was never written counts as absent), so a key the user changed stays the user's, and a key the
//! user left alone follows a changed default and is handed back when the default is unset.
//!
//! - Settings (`User/settings.json`, JSON with comments): `workbench.colorTheme`, with
//!   `window.autoDetectColorScheme` off while a theme is set, and `editor.fontSize`.
//! - Key bindings (`User/keybindings.json`, an array): each planned entry is added when the file
//!   has none with the same key, command and `when`, and not added again once the user removed it.
//! - Extensions: an id that is not installed and that Puddle did not install before is installed
//!   with the server's own `--install-extension` (an id the user uninstalled stays out).
//!
//! A file that does not parse is left as it is.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use puddle_agent_proto::editor::{EditorDefaults, EditorServer};
use puddle_types::{MergeEntry, MergeFormat, MergeSpec, Merged, parse_merge_file, unmerge};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::{CONFIG, EXTENSIONS, USER_DATA, withheld};
use crate::account::Account;

/// Merges the planned defaults before the server starts, hands what it made to `account` and
/// starts the extension installs in the background (a slow download never holds the server back).
pub(super) fn merge_at_start(
    server: &EditorServer,
    program: &Path,
    account: Option<&Account>,
) -> io::Result<()> {
    let done = apply(server);
    for path in &done.made {
        if let Some(account) = account {
            account.own(path)?;
        }
    }
    if !done.install.is_empty() {
        tokio::spawn(install_extensions(
            server.clone(),
            done.install,
            program.to_path_buf(),
            account.cloned(),
        ));
    }
    Ok(())
}

/// The state file's name, in the data folder.
const STATE: &str = "editor-defaults-applied.json";

const THEME: &str = "workbench.colorTheme";
const AUTO_DETECT: &str = "window.autoDetectColorScheme";
const FONT_SIZE: &str = "editor.fontSize";

/// What Puddle last wrote.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Applied {
    /// Setting key to the value Puddle last wrote (a key with no entry was never written, or was
    /// handed back).
    #[serde(default)]
    settings: BTreeMap<String, Value>,
    /// The key bindings Puddle added.
    #[serde(default)]
    keybindings: Vec<Value>,
    /// The extension ids Puddle saw installed or installed.
    #[serde(default)]
    extensions: Vec<String>,
}

/// What a run did, for the caller.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Outcome {
    /// Files and folders made or changed, for the caller to hand to the server's user.
    pub made: Vec<PathBuf>,
    /// The extension ids to install (lower case).
    pub install: Vec<String>,
}

fn state_path(server: &EditorServer) -> PathBuf {
    Path::new(&server.data).join(STATE)
}

fn user_dir(server: &EditorServer) -> PathBuf {
    Path::new(&server.data).join(USER_DATA).join("User")
}

fn read_applied(path: &Path) -> Applied {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_applied(path: &Path, applied: &Applied) -> io::Result<()> {
    let text = serde_json::to_string_pretty(applied).map_err(io::Error::other)?;
    puddle_fs::private::write_atomic(path, text.as_bytes())
}

/// The planned defaults; none when the file is missing, and none (with a warning) when it cannot be
/// read: the editor is not worth a failed start.
fn read_defaults(path: &str) -> EditorDefaults {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            tracing::warn!(%err, "the editor defaults file is not valid; no defaults are applied");
            EditorDefaults::default()
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => EditorDefaults::default(),
        Err(err) => {
            tracing::warn!(%err, "the editor defaults file cannot be read; no defaults are applied");
            EditorDefaults::default()
        }
    }
}

/// Merges the planned defaults into the editor's files. Never fails the start: a problem is logged
/// and the files stay as they are.
pub(super) fn apply(server: &EditorServer) -> Outcome {
    let defaults = read_defaults(&server.defaults_file);
    let state_file = state_path(server);
    let before = read_applied(&state_file);
    if defaults.is_empty() && before == Applied::default() {
        return Outcome::default();
    }
    let mut applied = before.clone();
    let mut outcome = Outcome::default();
    let user = user_dir(server);
    let result = (|| -> io::Result<()> {
        merge_settings(
            &user.join("settings.json"),
            &defaults,
            &mut applied,
            &mut outcome,
        )?;
        merge_keybindings(
            &user.join("keybindings.json"),
            &defaults,
            &mut applied,
            &mut outcome,
        )?;
        outcome.install = missing_extensions(server, &defaults, &mut applied);
        Ok(())
    })();
    if let Err(err) = result {
        tracing::warn!(%err, "the editor defaults could not be applied");
    }
    if applied != before {
        match write_applied(&state_file, &applied) {
            Ok(()) => outcome.made.push(state_file),
            Err(err) => tracing::warn!(%err, "the editor defaults record could not be written"),
        }
    }
    outcome
}

/// What Puddle wants each owned setting to be: `Some(value)` sets it, `None` removes it.
fn wanted_settings(
    defaults: &EditorDefaults,
    applied: &Applied,
) -> Vec<(&'static str, Option<Value>)> {
    let auto = if defaults.colour_theme.is_some() {
        Some(json!(false))
    } else if applied.settings.get(AUTO_DETECT) == Some(&json!(false)) {
        // The theme is unset again: hand the colour scheme back to the system, as the first open does.
        Some(json!(true))
    } else {
        applied.settings.get(AUTO_DETECT).cloned()
    };
    vec![
        (THEME, defaults.colour_theme.as_ref().map(|t| json!(t))),
        (AUTO_DETECT, auto),
        (FONT_SIZE, defaults.font_size.map(|px| json!(px))),
    ]
}

fn merge_settings(
    file: &Path,
    defaults: &EditorDefaults,
    applied: &mut Applied,
    outcome: &mut Outcome,
) -> io::Result<()> {
    let existing = read_optional(file)?;
    let current = match existing.as_deref() {
        None => Value::Object(serde_json::Map::new()),
        Some(bytes) => match parse_merge_file(MergeFormat::Jsonc, bytes) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(Value::Null) => Value::Object(serde_json::Map::new()),
            Ok(_) | Err(_) => {
                tracing::warn!(
                    "the editor settings file is not a JSON object; the defaults are not merged into it"
                );
                return Ok(());
            }
        },
    };
    let mut sets = Vec::new();
    let mut removes = Vec::new();
    for (key, wanted) in wanted_settings(defaults, applied) {
        let now = current.get(key);
        let last = applied.settings.get(key);
        // The first-open seed writes the colour scheme following; that is Puddle's own value.
        let seeded = key == AUTO_DETECT && last.is_none() && now == Some(&json!(true));
        if now != last && !seeded {
            continue;
        }
        if let Some(value) = wanted {
            if now != Some(&value) {
                sets.push(MergeEntry::json(&[key], &value));
            }
            applied.settings.insert(key.to_owned(), value);
        } else {
            if now.is_some() {
                removes.push(vec![key.to_owned()]);
            }
            applied.settings.remove(key);
        }
    }
    if sets.is_empty() && removes.is_empty() {
        return Ok(());
    }
    let mut text = existing;
    let refuse = |err: &dyn std::fmt::Display| {
        tracing::warn!(%err, "the editor settings file is left as it is");
    };
    if !sets.is_empty() {
        let spec = MergeSpec::new(MergeFormat::Jsonc, sets).map_err(io::Error::other)?;
        match spec.apply(text.as_deref(), &[]) {
            Ok(Merged::Write(bytes)) => text = Some(bytes),
            Ok(Merged::Unchanged) => {}
            Err(err) => {
                refuse(&err);
                return Ok(());
            }
        }
    }
    if !removes.is_empty()
        && let Some(bytes) = text.as_deref()
    {
        match unmerge(MergeFormat::Jsonc, bytes, &removes) {
            Ok(done) => {
                if let Merged::Write(out) = done.merged {
                    text = Some(out);
                }
            }
            Err(err) => {
                refuse(&err);
                return Ok(());
            }
        }
    }
    write_user_file(file, text.as_deref(), outcome)
}

/// One key binding's identity: its key, command and `when`.
fn identity(entry: &Value) -> Option<(String, String, String)> {
    let get = |k: &str| entry.get(k).and_then(Value::as_str).map(str::to_owned);
    Some((
        get("key")?,
        get("command")?,
        get("when").unwrap_or_default(),
    ))
}

fn planned_keybindings(defaults: &EditorDefaults) -> Vec<Value> {
    defaults
        .keybindings
        .as_deref()
        .and_then(|text| serde_json::from_str::<Vec<Value>>(text).ok())
        .unwrap_or_default()
}

fn merge_keybindings(
    file: &Path,
    defaults: &EditorDefaults,
    applied: &mut Applied,
    outcome: &mut Outcome,
) -> io::Result<()> {
    let wanted = planned_keybindings(defaults);
    if wanted.is_empty() && applied.keybindings.is_empty() {
        return Ok(());
    }
    let existing = read_optional(file)?;
    let entries: Vec<Value> = match existing.as_deref() {
        None => Vec::new(),
        Some(bytes) => match parse_merge_file(MergeFormat::Jsonc, bytes) {
            Ok(Value::Array(list)) => list,
            Ok(Value::Null) => Vec::new(),
            Ok(_) | Err(_) => {
                tracing::warn!(
                    "the editor key bindings file is not a JSON array; the defaults are not merged into it"
                );
                return Ok(());
            }
        },
    };
    let in_file = |entry: &Value| {
        let id = identity(entry);
        id.is_some() && entries.iter().any(|e| identity(e) == id)
    };
    let planned = |entry: &Value| wanted.iter().any(|w| identity(w) == identity(entry));
    let add: Vec<&Value> = wanted
        .iter()
        .filter(|w| {
            !in_file(w)
                && !applied
                    .keybindings
                    .iter()
                    .any(|a| identity(a) == identity(w))
        })
        .collect();
    let drop: Vec<&Value> = applied
        .keybindings
        .iter()
        .filter(|a| !planned(a) && in_file(a))
        .collect();
    // Dropping rewrites the array, which only a file without comments allows.
    let strict = existing
        .as_deref()
        .is_none_or(|bytes| serde_json::from_slice::<Value>(bytes).is_ok());
    let drop = if strict { drop } else { Vec::new() };
    let mut next_applied: Vec<Value> = wanted
        .iter()
        .filter(|w| {
            applied
                .keybindings
                .iter()
                .any(|a| identity(a) == identity(w))
                || add.iter().any(|a| identity(a) == identity(w))
        })
        .cloned()
        .collect();
    next_applied.extend(
        applied
            .keybindings
            .iter()
            .filter(|a| !planned(a) && in_file(a) && !strict)
            .cloned(),
    );
    if add.is_empty() && drop.is_empty() {
        applied.keybindings = next_applied;
        return Ok(());
    }
    let mut kept: Vec<Value> = entries
        .iter()
        .filter(|e| !drop.iter().any(|d| identity(d) == identity(e)))
        .cloned()
        .collect();
    let text = if let (Some(bytes), true) = (existing.as_deref(), drop.is_empty()) {
        // Added in place after the user's own entries, their comments untouched.
        let Some(out) = insert_entries(bytes, &add) else {
            tracing::warn!("the editor key bindings file is left as it is");
            return Ok(());
        };
        out
    } else {
        kept.extend(add.iter().map(|v| (*v).clone()));
        let mut out = serde_json::to_string_pretty(&kept).map_err(io::Error::other)?;
        out.push('\n');
        out.into_bytes()
    };
    // What goes out must read back as the expected list, or the file stays as it is.
    let expected_len = entries.len() - drop.len() + add.len();
    match parse_merge_file(MergeFormat::Jsonc, &text) {
        Ok(Value::Array(list)) if list.len() == expected_len => {}
        _ => {
            tracing::warn!(
                "the editor key bindings file is left as it is: the merge did not read back"
            );
            return Ok(());
        }
    }
    applied.keybindings = next_applied;
    write_user_file(file, Some(&text), outcome)
}

/// `bytes` (an array in JSON with comments) with `add` before its closing bracket.
fn insert_entries(bytes: &[u8], add: &[&Value]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let close = text.rfind(']')?;
    let before = text[..close].trim_end();
    let comma = if before.ends_with('[') || before.ends_with(',') {
        ""
    } else {
        ","
    };
    let list: Vec<String> = add
        .iter()
        .map(|v| serde_json::to_string(v).map(|s| format!("    {s}")))
        .collect::<Result<_, _>>()
        .ok()?;
    Some(format!("{before}{comma}\n{}\n{}", list.join(",\n"), &text[close..]).into_bytes())
}

/// The extension ids to install: wanted, not installed and not seen before. Ids that are no longer
/// wanted leave the record (an installed one stays installed).
fn missing_extensions(
    server: &EditorServer,
    defaults: &EditorDefaults,
    applied: &mut Applied,
) -> Vec<String> {
    let installed = installed_extensions(&Path::new(&server.data).join(EXTENSIONS));
    let wanted: Vec<String> = defaults
        .extensions
        .iter()
        .map(|id| id.to_lowercase())
        .collect();
    let mut install = Vec::new();
    for id in &wanted {
        let known = applied.extensions.contains(id);
        if installed
            .iter()
            .any(|name| name.starts_with(&format!("{id}-")))
        {
            if !known {
                applied.extensions.push(id.clone());
            }
        } else if !known {
            install.push(id.clone());
        }
    }
    applied.extensions.retain(|id| wanted.contains(id));
    install
}

/// The lower-cased folder names in the extensions folder (`publisher.name-version`).
fn installed_extensions(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| name.to_lowercase())
        .collect()
}

/// Records that Puddle installed `id`, for the next start not to install it again after the user
/// uninstalled it.
pub(super) fn record_installed(server: &EditorServer, id: &str) {
    let path = state_path(server);
    let mut applied = read_applied(&path);
    if !applied.extensions.iter().any(|known| known == id) {
        applied.extensions.push(id.to_owned());
        if let Err(err) = write_applied(&path, &applied) {
            tracing::warn!(%err, "the editor defaults record could not be written");
        }
    }
}

/// Installs the extensions of the editor defaults one after the other, as the server's own user,
/// and records each that worked. A failure is logged and tried again at the next start.
pub(super) async fn install_extensions(
    server: EditorServer,
    ids: Vec<String>,
    program: PathBuf,
    account: Option<Account>,
) {
    for id in ids {
        let data = Path::new(&server.data);
        let mut command = Command::new(&program);
        command
            .args(["--install-extension", &id, "--user-data-dir"])
            .arg(data.join(USER_DATA))
            .arg("--extensions-dir")
            .arg(data.join(EXTENSIONS))
            .arg("--config")
            .arg(data.join(CONFIG))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, _) in std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v))) {
            if withheld(&name) {
                command.env_remove(name);
            }
        }
        if let Some(account) = &account {
            command.envs(account.env());
            #[cfg(target_os = "linux")]
            account.run_as(&mut command);
        }
        let ran = match command.spawn() {
            Ok(mut child) => tokio::time::timeout(Duration::from_mins(5), child.wait())
                .await
                .map_err(|_| "it took more than five minutes".to_owned())
                .and_then(|status| status.map_err(|e| e.to_string())),
            Err(err) => Err(err.to_string()),
        };
        match ran {
            Ok(status) if status.success() => {
                tracing::info!(extension = %id, "an editor extension of the defaults is installed");
                record_installed(&server, &id);
            }
            Ok(status) => {
                tracing::warn!(extension = %id, %status, "an editor extension of the defaults did not install");
            }
            Err(reason) => {
                tracing::warn!(extension = %id, %reason, "an editor extension of the defaults did not install");
            }
        }
    }
}

fn read_optional(file: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(file) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// Writes `text` to `file` when it differs from what is there, keeping the mode of an existing
/// file, and notes what it made.
fn write_user_file(file: &Path, text: Option<&[u8]>, outcome: &mut Outcome) -> io::Result<()> {
    let Some(text) = text else {
        return Ok(());
    };
    if read_optional(file)?.as_deref() == Some(text) {
        return Ok(());
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
        outcome.made.push(dir.to_path_buf());
    }
    puddle_fs::private::write_atomic(file, text)?;
    outcome.made.push(file.to_path_buf());
    Ok(())
}

#[cfg(test)]
mod tests;
