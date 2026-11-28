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
//!   The user's own custom settings (`EditorDefaults::settings`, top-level keys) take the same path.
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
use puddle_types::{
    MergeEntry, MergeFormat, MergeRefusal, MergeSpec, Merged, ValidationError, parse_merge_file,
    unmerge,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use std::process::Stdio;

use super::{CONFIG, EXTENSIONS, Launch, USER_DATA, command};
use crate::account::Account;

/// Merges the planned defaults before the server starts, hands what it made to `account` and
/// starts the extension installs in the background (a slow download never holds the server back).
pub(super) fn merge_at_start(
    server: &EditorServer,
    program: &Path,
    account: Option<&Account>,
) -> io::Result<()> {
    let done = apply(server);
    account.map_or(Ok(()), |a| {
        done.made.iter().try_for_each(|path| a.own(path))
    })?;
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

/// How long one extension may take to install.
const INSTALL_BUDGET: std::time::Duration = std::time::Duration::from_mins(5);

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

/// Writes the record; a record that cannot be written costs only a repeat of work at the next
/// start, so it is a warning.
fn write_applied(path: &Path, applied: &Applied) {
    let text = serde_json::to_string_pretty(applied).unwrap_or_default();
    puddle_fs::private::write_atomic(path, text.as_bytes())
        .inspect_err(|err| tracing::warn!(%err, "the editor defaults record could not be written"))
        .ok();
}

/// The planned defaults; none when the file is missing or cannot be read, and none (with a
/// warning) when it is not valid: the editor is not worth a failed start.
fn read_defaults(path: &str) -> EditorDefaults {
    std::fs::read_to_string(path).map_or_else(
        |_| EditorDefaults::default(),
        |text| {
            serde_json::from_str(&text)
                .inspect_err(|err| tracing::warn!(%err, "the editor defaults file is not valid; no defaults are applied"))
                .unwrap_or_default()
        },
    )
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
    let mut run = Run {
        defaults: &defaults,
        applied: before.clone(),
        outcome: Outcome::default(),
    };
    let user = user_dir(server);
    run.settings(&user.join("settings.json"))
        .and_then(|()| run.keybindings(&user.join("keybindings.json")))
        .inspect_err(|err| tracing::warn!(%err, "the editor defaults could not be applied"))
        .ok();
    run.outcome.install = missing_extensions(server, &defaults, &mut run.applied);
    if run.applied != before {
        write_applied(&state_file, &run.applied);
        run.outcome.made.push(state_file);
    }
    run.outcome
}

/// One merge in progress: what is planned, what Puddle wrote before (updated as keys are
/// handled) and what was made.
struct Run<'a> {
    defaults: &'a EditorDefaults,
    applied: Applied,
    outcome: Outcome,
}

/// What Puddle wants each owned setting to be: `Some(value)` sets it, `None` removes it.
fn wanted_settings(defaults: &EditorDefaults, applied: &Applied) -> Vec<(String, Option<Value>)> {
    let auto = if defaults.colour_theme.is_some() {
        Some(json!(false))
    } else if applied.settings.get(AUTO_DETECT) == Some(&json!(false)) {
        // The theme is unset again: hand the colour scheme back to the system, as the first open does.
        Some(json!(true))
    } else {
        applied.settings.get(AUTO_DETECT).cloned()
    };
    let mut wanted = vec![
        (
            THEME.to_owned(),
            defaults.colour_theme.as_ref().map(|t| json!(t)),
        ),
        (AUTO_DETECT.to_owned(), auto),
        (FONT_SIZE.to_owned(), defaults.font_size.map(|px| json!(px))),
    ];
    let custom = planned_custom_settings(defaults);
    // A key Puddle wrote before that is no longer planned is handed back.
    let gone = applied
        .settings
        .keys()
        .filter(|k| !OWNED.contains(&k.as_str()) && !custom.contains_key(*k));
    wanted.extend(gone.map(|k| (k.clone(), None)));
    wanted.extend(custom.into_iter().map(|(k, v)| (k, Some(v))));
    wanted
}

/// The keys Puddle owns through their own fields; a custom entry for one is ignored.
const OWNED: [&str; 3] = [THEME, AUTO_DETECT, FONT_SIZE];

/// The user's own planned settings (a text that is not a JSON object plans none), without the
/// owned keys.
fn planned_custom_settings(defaults: &EditorDefaults) -> BTreeMap<String, Value> {
    defaults
        .settings
        .as_deref()
        .and_then(|text| serde_json::from_str::<BTreeMap<String, Value>>(text).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(k, _)| !OWNED.contains(&k.as_str()))
        .collect()
}

/// `bytes` as a JSON or JSONC value of one kind: `None` for a file that is not valid or holds
/// another kind (`want` says which); an empty file is an empty one.
fn read_as(bytes: Option<&[u8]>, want: fn(&Value) -> bool, empty: Value) -> Option<Value> {
    let Some(bytes) = bytes else {
        return Some(empty);
    };
    match parse_merge_file(MergeFormat::Jsonc, bytes) {
        Ok(Value::Null) => Some(empty),
        Ok(value) if want(&value) => Some(value),
        _ => None,
    }
}

impl Run<'_> {
    /// Merges the owned settings keys into `file`.
    fn settings(&mut self, file: &Path) -> io::Result<()> {
        let existing = read_optional(file)?;
        let Some(current) = read_as(existing.as_deref(), Value::is_object, json!({})) else {
            tracing::warn!(
                "the editor settings file is not a JSON object; the defaults are not merged into it"
            );
            return Ok(());
        };
        let mut sets = Vec::new();
        let mut removes = Vec::new();
        // Kept apart until the file holds the values: a refused or failed write must not be
        // remembered as done.
        let mut next = self.applied.settings.clone();
        for (key, wanted) in wanted_settings(self.defaults, &self.applied) {
            let key = key.as_str();
            let now = current.get(key);
            let last = self.applied.settings.get(key);
            // The first-open seed writes the colour scheme following; that is Puddle's own value.
            let seeded = key == AUTO_DETECT && last.is_none() && now == Some(&json!(true));
            if now != last && !seeded {
                continue;
            }
            if let Some(value) = wanted {
                sets.push(MergeEntry::json(&[key], &value));
                next.insert(key.to_owned(), value);
            } else {
                if now.is_some() {
                    removes.push(vec![key.to_owned()]);
                }
                next.remove(key);
            }
        }
        if sets.is_empty() && removes.is_empty() {
            self.applied.settings = next;
            return Ok(());
        }
        let edited = edit_settings(existing, &sets, &removes);
        match edited {
            Ok(Some(text)) => {
                self.write_user_file(file, &text)?;
                self.applied.settings = next;
                Ok(())
            }
            Ok(None) => {
                self.applied.settings = next;
                Ok(())
            }
            Err(err) => {
                tracing::warn!(%err, "the editor settings file is left as it is");
                Ok(())
            }
        }
    }

    /// Merges the planned key bindings into `file`.
    fn keybindings(&mut self, file: &Path) -> io::Result<()> {
        let wanted = planned_keybindings(self.defaults);
        if wanted.is_empty() && self.applied.keybindings.is_empty() {
            return Ok(());
        }
        // A file with only blanks is no file.
        let existing = read_optional(file)?.filter(|bytes| !bytes.trim_ascii().is_empty());
        let Some(Value::Array(entries)) = read_as(existing.as_deref(), Value::is_array, json!([]))
        else {
            tracing::warn!(
                "the editor key bindings file is not a JSON array; the defaults are not merged into it"
            );
            return Ok(());
        };
        let in_file = |entry: &Value| entries.iter().any(|e| identity(e) == identity(entry));
        let was =
            |list: &[Value], entry: &Value| list.iter().any(|a| identity(a) == identity(entry));
        let add: Vec<&Value> = wanted
            .iter()
            .filter(|w| !in_file(w) && !was(&self.applied.keybindings, w))
            .collect();
        // Dropping an entry rewrites the array, which only a file without comments allows.
        let strict = existing
            .as_deref()
            .is_none_or(|bytes| serde_json::from_slice::<Value>(bytes).is_ok());
        let drop: Vec<&Value> = self
            .applied
            .keybindings
            .iter()
            .filter(|a| !was(&wanted, a) && in_file(a) && strict)
            .collect();
        // What Puddle holds afterwards: the planned entries it added now or before, and the ones it
        // could not take out.
        let mut next: Vec<Value> = wanted
            .iter()
            .filter(|w| {
                was(&self.applied.keybindings, w) || add.iter().any(|a| identity(a) == identity(w))
            })
            .cloned()
            .collect();
        next.extend(
            self.applied
                .keybindings
                .iter()
                .filter(|a| !was(&wanted, a) && in_file(a) && !strict)
                .cloned(),
        );
        let text = if drop.is_empty() {
            insert_entries(existing.as_deref(), &add)
        } else {
            let mut kept: Vec<&Value> = entries
                .iter()
                .filter(|e| !drop.iter().any(|d| identity(d) == identity(e)))
                .collect();
            kept.extend(add.iter().copied());
            format!(
                "{}\n",
                serde_json::to_string_pretty(&kept).unwrap_or_default()
            )
        };
        if add.is_empty() && drop.is_empty() {
            self.applied.keybindings = next;
            return Ok(());
        }
        // What goes out must read back as the expected list, or the file stays as it is.
        let expected = entries.len() - drop.len() + add.len();
        let reads_back = matches!(
            parse_merge_file(MergeFormat::Jsonc, text.as_bytes()),
            Ok(Value::Array(list)) if list.len() == expected
        );
        if !reads_back {
            tracing::warn!(
                "the editor key bindings file is left as it is: the merge did not read back"
            );
            return Ok(());
        }
        self.write_user_file(file, text.as_bytes())?;
        self.applied.keybindings = next;
        Ok(())
    }

    /// Writes `text` to `file` and notes what it made.
    fn write_user_file(&mut self, file: &Path, text: &[u8]) -> io::Result<()> {
        let dir = file.parent().unwrap_or(file);
        std::fs::create_dir_all(dir)?;
        self.outcome.made.push(dir.to_path_buf());
        puddle_fs::private::write_atomic(file, text)?;
        self.outcome.made.push(file.to_path_buf());
        Ok(())
    }
}

/// `old` or, when `merged` writes, what it writes.
fn written(merged: Merged, old: Vec<u8>) -> Vec<u8> {
    match merged {
        Merged::Write(out) => out,
        Merged::Unchanged => old,
    }
}

/// Why the editor's settings file is left as it is.
#[derive(Debug, thiserror::Error)]
enum SettingsRefused {
    /// The entries to write are not a valid merge.
    #[error(transparent)]
    Spec(#[from] ValidationError),
    /// The file cannot be merged into or edited.
    #[error(transparent)]
    File(#[from] MergeRefusal),
}

/// `existing` with `sets` written and `removes` taken out: `None` when that changes nothing, or
/// why the file is left alone.
fn edit_settings(
    existing: Option<Vec<u8>>,
    sets: &[MergeEntry],
    removes: &[Vec<String>],
) -> Result<Option<Vec<u8>>, SettingsRefused> {
    let old = existing.unwrap_or_default();
    let mut text = old.clone();
    if !sets.is_empty() {
        text = with_sets(text, sets)?;
    }
    if !removes.is_empty() {
        let gone = unmerge(MergeFormat::Jsonc, &text, removes)?;
        text = written(gone.merged, text);
    }
    Ok((text != old).then_some(text))
}

/// `text` with `sets` merged in, or why the file is left alone.
fn with_sets(text: Vec<u8>, sets: &[MergeEntry]) -> Result<Vec<u8>, SettingsRefused> {
    let spec = MergeSpec::new(MergeFormat::Jsonc, sets.to_vec())?;
    let from = (!text.is_empty()).then_some(text.as_slice());
    let merged = spec.apply(from, &[])?;
    Ok(written(merged, text))
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

/// `existing` (an array in JSON with comments, or no file) with `add` before its closing bracket.
fn insert_entries(existing: Option<&[u8]>, add: &[&Value]) -> String {
    let list: Vec<String> = add.iter().map(|v| format!("    {v}")).collect();
    let Some(bytes) = existing else {
        return format!("[\n{}\n]\n", list.join(",\n"));
    };
    let text = String::from_utf8_lossy(bytes);
    let close = text.rfind(']').unwrap_or(text.len());
    let before = text[..close].trim_end();
    let comma = if before.ends_with(['[', ',']) {
        ""
    } else {
        ","
    };
    format!("{before}{comma}\n{}\n{}", list.join(",\n"), &text[close..])
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
        if installed.iter().any(|name| is_folder_of(name, id)) {
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

/// Whether the folder `name` is an install of extension `id`: `<id>-<version>`, where the version
/// starts with a digit, so `acme.tool-extra-1.0.0` is not `acme.tool`.
fn is_folder_of(name: &str, id: &str) -> bool {
    name.strip_prefix(id)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|version| version.starts_with(|c: char| c.is_ascii_digit()))
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
        write_applied(&path, &applied);
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
    let data = Path::new(&server.data);
    for id in ids {
        let launch = Launch {
            program: program.clone(),
            args: vec![
                "--install-extension".into(),
                id.as_str().into(),
                "--user-data-dir".into(),
                data.join(USER_DATA).into(),
                "--extensions-dir".into(),
                data.join(EXTENSIONS).into(),
                "--config".into(),
                data.join(CONFIG).into(),
            ],
            password_env: false,
            build: None,
        };
        let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
        let mut command = command(&launch, None, inherited, account.as_ref());
        command.stdout(Stdio::null()).stderr(Stdio::null());
        let status = match command.spawn() {
            Ok(mut child) => tokio::time::timeout(INSTALL_BUDGET, child.wait())
                .await
                .map_err(|_| "it took too long".to_owned())
                .and_then(|status| status.map_err(|e| e.to_string())),
            Err(err) => Err(err.to_string()),
        };
        match status {
            Ok(status) if status.success() => record_installed(&server, &id),
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

#[cfg(test)]
mod tests;
