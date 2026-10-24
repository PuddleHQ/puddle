// SPDX-License-Identifier: GPL-3.0-or-later
//! Puddle's ssh config: a file of its own with one host entry per workspace that has direct SSH
//! on and a live endpoint, and the single `Include` line that pulls it into the user's own
//! `~/.ssh/config`.
//!
//! Puddle rewrites only its own file, whole, from what is live in this process: an entry is there
//! while the workspace's endpoint is, so a stale entry never outlives a crash (the file is
//! emptied when the host starts). The user's own file is touched for the one `Include` line
//! and nothing else, and only once an entry is about to be written, which is after the user
//! turned direct SSH on and saw what that trusts.
//!
//! Every value written comes from Puddle (a workspace name, which is a DNS label, the data folder,
//! the program and the endpoint's name), never from a guest. The writer still refuses values that
//! could end a quoted word or start a shell expansion, so no value can become a second line.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use puddle_types::WorkspaceName;

use crate::ssh_login::{LoginKey, SSH_DIR};

const CONFIG_FILE: &str = "config";
const KNOWN_HOSTS_DIR: &str = "known_hosts";

/// Where the ssh pieces of the host come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SshSettings {
    /// The `puddle` program that `ssh` runs as its `ProxyCommand` (`puddle ssh-bridge`). `None`
    /// leaves direct SSH without entries: the endpoint still opens, but no ssh config can name it.
    pub bridge_program: Option<PathBuf>,
    /// The user's own ssh config, which gets the one `Include` line. `None` writes Puddle's file
    /// only.
    pub user_config: Option<PathBuf>,
}

/// The name of a workspace's entry in the ssh config: what `ssh` and Remote-SSH connect to.
#[must_use]
pub fn ssh_host(workspace: &WorkspaceName) -> String {
    format!("puddle-{workspace}")
}

/// What the ssh config needs and the entries that are live now.
#[derive(Debug)]
pub(crate) struct SshSetup {
    dir: PathBuf,
    login: Result<LoginKey, String>,
    bridge: Option<PathBuf>,
    user_config: Option<PathBuf>,
    entries: Mutex<BTreeMap<WorkspaceName, PathBuf>>,
}

impl SshSetup {
    /// The setup under `data`: the login key (made if needed) and an emptied config file, since
    /// no endpoint is open yet. A failure to make the key is kept and said when it matters.
    pub(crate) fn open(data: &Path, settings: &SshSettings) -> Self {
        let dir = data.join(SSH_DIR);
        let login = puddle_fs::private::create_dir(&dir)
            .and_then(|()| LoginKey::open(&dir))
            .map_err(|e| format!("cannot make the login key in {}: {e}", dir.display()));
        let setup = Self {
            dir,
            login,
            bridge: settings.bridge_program.clone(),
            user_config: settings.user_config.clone(),
            entries: Mutex::new(BTreeMap::new()),
        };
        if setup.config_path().is_file()
            && let Err(e) = setup.write_config(&BTreeMap::new())
        {
            tracing::warn!(error = %e, "cannot empty Puddle's ssh config at start");
        }
        setup
    }

    /// The public login key the runtime's SSH server accepts, as an OpenSSH line.
    pub(crate) fn authorized_key(&self) -> Option<&str> {
        self.login.as_ref().ok().map(LoginKey::public_line)
    }

    /// Puddle's own ssh config file.
    pub(crate) fn config_path(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }

    fn known_hosts(&self, workspace: &WorkspaceName) -> PathBuf {
        self.dir.join(KNOWN_HOSTS_DIR).join(workspace.as_str())
    }

    /// Makes `workspace`'s entry name `endpoint` (the caller has checked the direct SSH gate),
    /// and the user's ssh config include Puddle's file. The workspace's host key is forgotten
    /// when the endpoint is a new one, and kept when it is the endpoint the entry already names.
    ///
    /// # Errors
    ///
    /// Why the entry could not be written, for the user.
    pub(crate) fn publish(&self, workspace: &WorkspaceName, endpoint: &Path) -> Result<(), String> {
        let login = self.login.as_ref().map_err(Clone::clone)?;
        let bridge = self.bridge.as_deref().ok_or_else(|| {
            "this Puddle has no `puddle` program to run as the ssh bridge".to_owned()
        })?;
        let known_hosts = self.known_hosts(workspace);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let before = entries.insert(workspace.clone(), endpoint.to_path_buf());
        let context = Context {
            bridge,
            identity: login.private_path(),
            known_hosts_dir: &self.dir.join(KNOWN_HOSTS_DIR),
        };
        // A sandbox made anew has a new endpoint and a new host key, and a key left over would stop
        // `ssh` from connecting. The same endpoint is the same sandbox: its key stays, so every
        // later attach is checked against it.
        let endpoint_changed = before.as_deref() != Some(endpoint);
        let written = render(&entries, &context).and_then(|text| {
            if endpoint_changed {
                match std::fs::remove_file(&known_hosts) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("cannot forget the old host key: {e}")),
                }
            }
            puddle_fs::private::create_dir(context.known_hosts_dir)
                .and_then(|()| {
                    puddle_fs::private::write_atomic(&self.config_path(), text.as_bytes())
                })
                .map_err(|e| format!("cannot write {}: {e}", self.config_path().display()))
        });
        if let Err(e) = written {
            // Nothing was written: the file still matches what was there before.
            match before {
                Some(old) => entries.insert(workspace.clone(), old),
                None => entries.remove(workspace),
            };
            return Err(e);
        }
        // The entry is in Puddle's file whether or not the user's file could take the include.
        self.include()
    }

    /// Takes `workspace`'s entry out of the file. Nothing is written when it had none.
    pub(crate) fn withdraw(&self, workspace: &WorkspaceName) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.remove(workspace).is_none() {
            return;
        }
        if let Err(e) = self.write_config(&entries) {
            tracing::warn!(%workspace, error = %e, "cannot take the workspace out of Puddle's ssh config");
        }
        let _ = std::fs::remove_file(self.known_hosts(workspace));
    }

    /// The endpoint `workspace`'s entry names now.
    #[cfg(all(test, unix))]
    pub(crate) fn endpoint(&self, workspace: &WorkspaceName) -> Option<PathBuf> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .cloned()
    }

    fn write_config(&self, entries: &BTreeMap<WorkspaceName, PathBuf>) -> Result<(), String> {
        let (Ok(login), Some(bridge)) = (&self.login, &self.bridge) else {
            return puddle_fs::private::write_atomic(&self.config_path(), HEADER.as_bytes())
                .map_err(|e| e.to_string());
        };
        let context = Context {
            bridge,
            identity: login.private_path(),
            known_hosts_dir: &self.dir.join(KNOWN_HOSTS_DIR),
        };
        let text = render(entries, &context)?;
        puddle_fs::private::write_atomic(&self.config_path(), text.as_bytes())
            .map_err(|e| e.to_string())
    }

    fn include(&self) -> Result<(), String> {
        let Some(user) = &self.user_config else {
            return Ok(());
        };
        ensure_include(user, &self.config_path()).map_err(|e| {
            format!(
                "cannot add Puddle's include line to {}: {e}",
                user.display()
            )
        })
    }
}

const HEADER: &str = "# Written by Puddle, and rewritten whenever a workspace's direct SSH starts or stops.\n\
                      # Do not edit: changes are lost. Puddle adds only an Include line for this file to\n\
                      # your own ssh config.\n";

/// What an entry names besides the workspace.
struct Context<'a> {
    bridge: &'a Path,
    identity: &'a Path,
    known_hosts_dir: &'a Path,
}

/// The whole of Puddle's ssh config for `entries`.
fn render(
    entries: &BTreeMap<WorkspaceName, PathBuf>,
    context: &Context<'_>,
) -> Result<String, String> {
    let mut text = HEADER.to_owned();
    for (workspace, endpoint) in entries {
        let host = ssh_host(workspace);
        let identity = quoted(context.identity)?;
        let known_hosts = quoted(&context.known_hosts_dir.join(workspace.as_str()))?;
        let proxy = proxy_command(context.bridge, endpoint)?;
        let _ = write!(
            text,
            "\nHost {host}\n\
             \x20   HostName {host}\n\
             \x20   User root\n\
             \x20   ProxyCommand {proxy}\n\
             \x20   IdentityFile {identity}\n\
             \x20   IdentitiesOnly yes\n\
             \x20   IdentityAgent none\n\
             \x20   PreferredAuthentications publickey\n\
             \x20   PasswordAuthentication no\n\
             \x20   KbdInteractiveAuthentication no\n\
             \x20   ForwardAgent no\n\
             \x20   ForwardX11 no\n\
             \x20   ControlMaster no\n\
             \x20   ControlPath none\n\
             \x20   UserKnownHostsFile {known_hosts}\n\
             \x20   HostKeyAlias {host}\n\
             \x20   StrictHostKeyChecking accept-new\n"
        );
    }
    Ok(text)
}

/// A value that is safe inside double quotes in an ssh config and inside the shell or `cmd`
/// that runs a `ProxyCommand`: nothing that ends the word, expands, or starts a second line.
fn checked(value: &str) -> Result<&str, String> {
    let forbidden = |c: char| {
        c.is_control() || matches!(c, '"' | '%' | '$' | '`') || (!cfg!(windows) && c == '\\')
    };
    match value.chars().find(|&c| forbidden(c)) {
        None => Ok(value),
        Some(c) => Err(format!(
            "{value:?} has the character {c:?}, which an ssh config cannot hold safely"
        )),
    }
}

/// `path` as a double-quoted ssh config word, with forward slashes on Windows (OpenSSH reads a
/// backslash as an escape in some places).
fn quoted(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.to_owned()
    };
    Ok(format!("\"{}\"", checked(&text)?))
}

/// `"<bridge>" ssh-bridge "<endpoint>"`: the rest of the line is the command, which the shell
/// (or `cmd` on Windows) reads, so the endpoint keeps its backslashes (`\\.\pipe\...`).
fn proxy_command(bridge: &Path, endpoint: &Path) -> Result<String, String> {
    let word = |p: &Path| -> Result<String, String> {
        let text = p
            .to_str()
            .ok_or_else(|| format!("{} is not valid UTF-8", p.display()))?;
        Ok(format!("\"{}\"", checked(text)?))
    };
    Ok(format!("{} ssh-bridge {}", word(bridge)?, word(endpoint)?))
}

/// Makes the file `user` start with `Include "<puddle's file>"`, changing nothing else in it.
/// A symlinked config is followed, the file's permissions stay, and a line that is already there
/// at the top level (before the first `Host` or `Match`, where `ssh` applies it to every host) is
/// enough. The file is not touched when it is read-only, when it is a link to a file that is not
/// there, or when something else saved it while Puddle was editing.
///
/// # Errors
///
/// Any file-system error, one of the refusals above, or a path that cannot be quoted.
pub(crate) fn ensure_include(user: &Path, puddle: &Path) -> Result<(), String> {
    let line = format!("Include {}", quoted(puddle)?);
    let target = match std::fs::canonicalize(user) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let is_link = std::fs::symlink_metadata(user).is_ok_and(|m| m.is_symlink());
            if is_link {
                return Err(format!(
                    "{} is a symbolic link to a file that is not there, and Puddle will not replace the link",
                    user.display()
                ));
            }
            user.to_path_buf()
        }
        Err(e) => return Err(e.to_string()),
    };
    let (existing, seen) = match std::fs::read(&target) {
        Ok(bytes) => {
            let meta = std::fs::metadata(&target).map_err(|e| e.to_string())?;
            (Some(bytes), Some(meta))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (None, None),
        Err(e) => return Err(e.to_string()),
    };
    let old = existing.as_deref().unwrap_or_default();
    if has_top_level_line(old, line.as_bytes()) {
        return Ok(());
    }
    let eol: &[u8] = if old.windows(2).any(|w| w == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let mut new = line.into_bytes();
    new.extend_from_slice(eol);
    if !old.is_empty() {
        new.extend_from_slice(eol);
        new.extend_from_slice(old);
    }
    let Some(seen) = seen else {
        return puddle_fs::private::write_atomic(&target, &new).map_err(|e| e.to_string());
    };
    if seen.permissions().readonly() {
        return Err(format!(
            "{} is read-only, and Puddle will not change it",
            target.display()
        ));
    }
    replace_keeping_permissions(&target, &new, &seen).map_err(|e| e.to_string())
}

/// Whether `line` is in `old` before its first `Host` or `Match` line: after one, `ssh` applies
/// an `Include` only to the hosts that block matches.
fn has_top_level_line(old: &[u8], line: &[u8]) -> bool {
    for l in old.split(|&b| b == b'\n') {
        let l = l.trim_ascii();
        if l == line {
            return true;
        }
        let keyword = l
            .split(|b| b.is_ascii_whitespace() || *b == b'=')
            .next()
            .unwrap_or_default();
        if keyword.eq_ignore_ascii_case(b"Host") || keyword.eq_ignore_ascii_case(b"Match") {
            return false;
        }
    }
    false
}

/// Replaces `target` with `body`, with the permissions it has, through a temporary copy in the
/// same folder. `seen` is what `target` was when `body` was made: if its length or time of
/// change differs just before the swap, another program saved it meanwhile and its edit would be
/// lost, so nothing is replaced.
fn replace_keeping_permissions(
    target: &Path,
    body: &[u8],
    seen: &std::fs::Metadata,
) -> io::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let mut suffix = [0u8; 6];
    getrandom::fill(&mut suffix).map_err(|e| io::Error::other(e.to_string()))?;
    let tmp = dir.join(format!(
        ".puddle-config-{:012x}.tmp",
        u64::from_be_bytes([
            0, 0, suffix[0], suffix[1], suffix[2], suffix[3], suffix[4], suffix[5]
        ])
    ));
    let result = (|| {
        // Owner-only from the first byte; the file's own permissions go on when it is complete.
        let mut file = puddle_fs::private::create_file(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        std::fs::set_permissions(&tmp, seen.permissions())?;
        let now = std::fs::metadata(target)?;
        if now.len() != seen.len() || now.modified()? != seen.modified()? {
            return Err(io::Error::other(format!(
                "{} was changed by another program meanwhile",
                target.display()
            )));
        }
        puddle_fs::private::replace_file(&tmp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> WorkspaceName {
        WorkspaceName::new(n).unwrap()
    }

    fn context<'a>(bridge: &'a Path, identity: &'a Path, dir: &'a Path) -> Context<'a> {
        Context {
            bridge,
            identity,
            known_hosts_dir: dir,
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_names_the_bridge_the_endpoint_and_only_puddles_key() {
        let mut entries = BTreeMap::new();
        entries.insert(name("web"), PathBuf::from("/run/puddle/ep-1"));
        let text = render(
            &entries,
            &context(
                Path::new("/opt/puddle/puddle"),
                Path::new("/data/ssh/login_ed25519"),
                Path::new("/data/ssh/known_hosts"),
            ),
        )
        .unwrap();
        for want in [
            "Host puddle-web\n",
            "    User root\n",
            "    ProxyCommand \"/opt/puddle/puddle\" ssh-bridge \"/run/puddle/ep-1\"\n",
            "    IdentityFile \"/data/ssh/login_ed25519\"\n",
            "    IdentitiesOnly yes\n",
            "    IdentityAgent none\n",
            "    ControlMaster no\n",
            "    ControlPath none\n",
            "    UserKnownHostsFile \"/data/ssh/known_hosts/web\"\n",
            "    HostKeyAlias puddle-web\n",
        ] {
            assert!(text.contains(want), "{want:?} in\n{text}");
        }
        assert_eq!(text.matches("Host puddle-").count(), 1);
    }

    #[test]
    fn no_entries_is_just_the_header() {
        let text = render(
            &BTreeMap::new(),
            &context(Path::new("/p"), Path::new("/k"), Path::new("/h")),
        )
        .unwrap();
        assert_eq!(text, HEADER);
        assert!(!text.contains("Host "));
    }

    #[test]
    fn a_value_that_could_end_a_word_or_start_a_line_is_refused() {
        let ok = context(Path::new("/p"), Path::new("/k"), Path::new("/h"));
        for hostile in [
            "/tmp/a\"b",
            "/tmp/a\nHost evil",
            "/tmp/a\rProxyCommand x",
            "/tmp/$(id)",
            "/tmp/`id`",
            "/tmp/%h",
            "/tmp/a\0b",
        ] {
            let mut entries = BTreeMap::new();
            entries.insert(name("web"), PathBuf::from(hostile));
            let err = render(&entries, &ok).unwrap_err();
            assert!(err.contains("cannot hold safely"), "{hostile:?}: {err}");
            let identity = context(Path::new("/p"), Path::new(hostile), Path::new("/h"));
            let mut entries = BTreeMap::new();
            entries.insert(name("web"), PathBuf::from("/ep"));
            assert!(render(&entries, &identity).is_err(), "identity {hostile:?}");
            let program = context(Path::new(hostile), Path::new("/k"), Path::new("/h"));
            assert!(render(&entries, &program).is_err(), "program {hostile:?}");
        }
    }

    #[cfg(unix)]
    mod files {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;

        fn setup(dir: &Path, with_user: bool) -> (SshSetup, PathBuf) {
            let user = dir.join("home/.ssh/config");
            let settings = SshSettings {
                bridge_program: Some(PathBuf::from("/opt/puddle/puddle")),
                user_config: with_user.then(|| user.clone()),
            };
            (SshSetup::open(&dir.join("data"), &settings), user)
        }

        #[test]
        fn publishing_writes_the_entry_and_one_include_line_and_withdrawing_removes_the_entry() {
            let dir = tempfile::tempdir().unwrap();
            let (ssh, user) = setup(dir.path(), true);
            assert!(ssh.authorized_key().is_some());
            ssh.publish(&name("web"), Path::new("/run/ep-1")).unwrap();
            let puddle_file = std::fs::read_to_string(ssh.config_path()).unwrap();
            assert!(puddle_file.contains("Host puddle-web"));
            let user_text = std::fs::read_to_string(&user).unwrap();
            assert_eq!(
                user_text,
                format!("Include \"{}\"\n", ssh.config_path().display())
            );
            ssh.withdraw(&name("web"));
            assert!(
                !std::fs::read_to_string(ssh.config_path())
                    .unwrap()
                    .contains("Host ")
            );
            assert_eq!(std::fs::read_to_string(&user).unwrap(), user_text);
            assert_eq!(ssh.endpoint(&name("web")), None);
        }

        #[test]
        fn an_entry_follows_its_endpoint_and_forgets_the_old_host_key() {
            let dir = tempfile::tempdir().unwrap();
            let (ssh, _) = setup(dir.path(), false);
            ssh.publish(&name("web"), Path::new("/run/ep-1")).unwrap();
            let known = ssh.known_hosts(&name("web"));
            std::fs::write(&known, "puddle-web ssh-ed25519 AAAA old\n").unwrap();
            ssh.publish(&name("web"), Path::new("/run/ep-2")).unwrap();
            assert!(!known.exists());
            let text = std::fs::read_to_string(ssh.config_path()).unwrap();
            assert!(
                text.contains("/run/ep-2") && !text.contains("/run/ep-1"),
                "{text}"
            );
        }

        #[test]
        fn a_stale_file_from_a_crashed_run_is_emptied_at_start() {
            let dir = tempfile::tempdir().unwrap();
            let (ssh, _) = setup(dir.path(), false);
            ssh.publish(&name("web"), Path::new("/run/ep-1")).unwrap();
            drop(ssh);
            let (again, _) = setup(dir.path(), false);
            assert!(
                !std::fs::read_to_string(again.config_path())
                    .unwrap()
                    .contains("Host ")
            );
        }

        #[test]
        fn a_failed_publish_leaves_the_entries_as_they_were() {
            let dir = tempfile::tempdir().unwrap();
            let (ssh, _) = setup(dir.path(), false);
            ssh.publish(&name("web"), Path::new("/run/ep-1")).unwrap();
            let err = ssh
                .publish(&name("web"), Path::new("/run/\"x"))
                .unwrap_err();
            assert!(err.contains("cannot hold"));
            assert_eq!(ssh.endpoint(&name("web")), Some(PathBuf::from("/run/ep-1")));
            assert!(ssh.publish(&name("db"), Path::new("/run/\n")).is_err());
            assert_eq!(ssh.endpoint(&name("db")), None);
        }

        #[test]
        fn a_host_key_file_that_cannot_be_removed_is_an_error_and_changes_nothing() {
            let dir = tempfile::tempdir().unwrap();
            let (ssh, _) = setup(dir.path(), false);
            // A folder where the file should be: removing it as a file fails.
            std::fs::create_dir_all(ssh.known_hosts(&name("web"))).unwrap();
            let err = ssh.publish(&name("web"), Path::new("/run/ep")).unwrap_err();
            assert!(err.contains("cannot forget the old host key"), "{err}");
            assert_eq!(ssh.endpoint(&name("web")), None);
        }

        #[test]
        fn a_start_without_key_or_program_still_empties_the_old_file() {
            let dir = tempfile::tempdir().unwrap();
            let data = dir.path().join("data");
            let (first, _) = setup(dir.path(), false);
            first.publish(&name("web"), Path::new("/run/ep")).unwrap();
            drop(first);
            let again = SshSetup::open(&data, &SshSettings::default());
            assert_eq!(
                std::fs::read_to_string(again.config_path()).unwrap(),
                HEADER
            );
        }

        #[test]
        fn a_config_path_below_a_file_is_an_error() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("file");
            std::fs::write(&file, "x").unwrap();
            let err = ensure_include(&file.join("config"), &dir.path().join("p")).unwrap_err();
            assert!(err.contains("os error"), "{err}");
        }

        #[test]
        fn without_a_bridge_program_there_is_no_entry_and_it_says_why() {
            let dir = tempfile::tempdir().unwrap();
            let ssh = SshSetup::open(&dir.path().join("data"), &SshSettings::default());
            let err = ssh.publish(&name("web"), Path::new("/run/ep")).unwrap_err();
            assert!(err.contains("no `puddle` program"), "{err}");
        }

        #[test]
        fn an_unmakeable_key_is_kept_and_blocks_entries() {
            let dir = tempfile::tempdir().unwrap();
            let blocker = dir.path().join("data");
            std::fs::write(&blocker, "a file where the folder should be").unwrap();
            let settings = SshSettings {
                bridge_program: Some("/p".into()),
                user_config: None,
            };
            let ssh = SshSetup::open(&blocker, &settings);
            assert!(ssh.authorized_key().is_none());
            let err = ssh.publish(&name("web"), Path::new("/e")).unwrap_err();
            assert!(err.contains("login key"), "{err}");
        }

        #[test]
        fn the_include_goes_first_and_the_rest_of_the_users_file_is_byte_for_byte_the_same() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            let original = b"Host *\n  ServerAliveInterval 30\r\n\xff not utf8\n";
            std::fs::write(&user, original).unwrap();
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o640)).unwrap();
            let puddle = dir.path().join("own ssh config");
            ensure_include(&user, &puddle).unwrap();
            let now = std::fs::read(&user).unwrap();
            let line = format!("Include \"{}\"", puddle.display());
            assert!(now.starts_with(line.as_bytes()));
            assert!(
                now.ends_with(original),
                "{:?}",
                String::from_utf8_lossy(&now)
            );
            // The file has a CRLF, so the new line and the blank line after it end with one too.
            assert_eq!(now.len(), line.len() + 4 + original.len());
            assert_eq!(
                std::fs::metadata(&user).unwrap().permissions().mode() & 0o777,
                0o640
            );
            ensure_include(&user, &puddle).unwrap();
            assert_eq!(
                std::fs::read(&user).unwrap(),
                now,
                "a second call changes nothing"
            );
        }

        #[test]
        fn a_crlf_file_gets_a_crlf_line() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            std::fs::write(&user, "Host a\r\n  User x\r\n").unwrap();
            ensure_include(&user, &dir.path().join("p")).unwrap();
            let text = std::fs::read_to_string(&user).unwrap();
            assert!(text.contains("\"\r\n\r\nHost a"), "{text:?}");
        }

        #[test]
        fn a_symlinked_config_stays_a_symlink_and_its_target_changes() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("dotfiles-config");
            std::fs::write(&real, "Host a\n").unwrap();
            let link = dir.path().join("config");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            ensure_include(&link, &dir.path().join("p")).unwrap();
            assert!(
                std::fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(
                std::fs::read_to_string(&real)
                    .unwrap()
                    .starts_with("Include ")
            );
        }

        #[test]
        fn a_missing_config_and_folder_are_made_owner_only() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join(".ssh/config");
            ensure_include(&user, &dir.path().join("p")).unwrap();
            assert_eq!(
                std::fs::metadata(&user).unwrap().permissions().mode() & 0o077,
                0
            );
            assert_eq!(
                std::fs::metadata(user.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }

        #[test]
        fn a_top_level_include_the_user_already_has_is_enough_even_when_it_is_not_first() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            let puddle = dir.path().join("p");
            let text = format!(
                "# mine\nSendEnv LANG\n  Include \"{}\"\nHost a\n  User x\n",
                puddle.display()
            );
            std::fs::write(&user, &text).unwrap();
            ensure_include(&user, &puddle).unwrap();
            assert_eq!(std::fs::read_to_string(&user).unwrap(), text);
        }

        #[test]
        fn an_include_inside_a_host_or_match_block_does_not_count() {
            let dir = tempfile::tempdir().unwrap();
            let puddle = dir.path().join("p");
            let line = format!("Include \"{}\"", puddle.display());
            for opener in [
                "Host a",
                "host a",
                "HOST=a",
                "Host\ta",
                "Match all",
                "match host a",
                "  Host *",
            ] {
                let user = dir.path().join("config");
                let original = format!("{opener}\n  {line}\n  User x\n");
                std::fs::write(&user, &original).unwrap();
                ensure_include(&user, &puddle).unwrap();
                let now = std::fs::read_to_string(&user).unwrap();
                assert_eq!(now, format!("{line}\n\n{original}"), "{opener}");
                ensure_include(&user, &puddle).unwrap();
                assert_eq!(std::fs::read_to_string(&user).unwrap(), now, "{opener}");
            }
        }

        #[test]
        fn a_word_that_only_starts_like_host_does_not_end_the_top_level() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            let puddle = dir.path().join("p");
            let text = format!(
                "HostbasedAuthentication no\nMatchless yes\nInclude \"{}\"\n",
                puddle.display()
            );
            std::fs::write(&user, &text).unwrap();
            ensure_include(&user, &puddle).unwrap();
            assert_eq!(std::fs::read_to_string(&user).unwrap(), text);
        }

        #[test]
        fn a_link_to_a_file_that_is_not_there_stays_and_is_an_error() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("config");
            let missing = dir.path().join("dotfiles/config");
            std::os::unix::fs::symlink(&missing, &link).unwrap();
            let err = ensure_include(&link, &dir.path().join("p")).unwrap_err();
            assert!(err.contains("symbolic link"), "{err}");
            assert_eq!(std::fs::read_link(&link).unwrap(), missing);
            assert!(!missing.exists());
        }

        #[test]
        fn a_read_only_config_is_not_rewritten_but_one_that_has_the_line_is_fine() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            let puddle = dir.path().join("p");
            std::fs::write(&user, "Host a\n").unwrap();
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o444)).unwrap();
            let err = ensure_include(&user, &puddle).unwrap_err();
            assert!(err.contains("read-only"), "{err}");
            assert_eq!(std::fs::read_to_string(&user).unwrap(), "Host a\n");
            assert_eq!(
                std::fs::metadata(&user).unwrap().permissions().mode() & 0o777,
                0o444
            );
            let text = format!("Include \"{}\"\n", puddle.display());
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::write(&user, &text).unwrap();
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o444)).unwrap();
            ensure_include(&user, &puddle).unwrap();
        }

        #[test]
        fn a_config_saved_by_another_program_meanwhile_is_not_replaced() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            std::fs::write(&user, "Host a\n").unwrap();
            let seen = std::fs::metadata(&user).unwrap();
            // The other program's save: a different length.
            std::fs::write(&user, "Host a\nHost b\n").unwrap();
            let err = replace_keeping_permissions(&user, b"Include x\n", &seen).unwrap_err();
            assert!(
                err.to_string().contains("changed by another program"),
                "{err}"
            );
            assert_eq!(std::fs::read_to_string(&user).unwrap(), "Host a\nHost b\n");
            // Same length, later time.
            let seen = std::fs::metadata(&user).unwrap();
            std::fs::write(&user, "Host a\nHost c\n").unwrap();
            let later = seen.modified().unwrap() + std::time::Duration::from_secs(5);
            std::fs::File::options()
                .write(true)
                .open(&user)
                .unwrap()
                .set_modified(later)
                .unwrap();
            assert!(replace_keeping_permissions(&user, b"Include x\n", &seen).is_err());
            assert_eq!(std::fs::read_to_string(&user).unwrap(), "Host a\nHost c\n");
            let leftovers: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(leftovers.len(), 1, "{leftovers:?}");
        }

        #[test]
        fn the_swap_keeps_the_files_own_mode_even_a_wide_one() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            std::fs::write(&user, "Host a\n").unwrap();
            std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o644)).unwrap();
            ensure_include(&user, &dir.path().join("p")).unwrap();
            assert_eq!(
                std::fs::metadata(&user).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }

        #[test]
        fn the_same_endpoint_keeps_the_host_key_and_a_new_one_forgets_it() {
            let dir = tempfile::tempdir().unwrap();
            let (ssh, _) = setup(dir.path(), false);
            ssh.publish(&name("web"), Path::new("/run/ep-1")).unwrap();
            let known = ssh.known_hosts(&name("web"));
            std::fs::write(&known, "puddle-web ssh-ed25519 AAAA key\n").unwrap();
            ssh.publish(&name("web"), Path::new("/run/ep-1")).unwrap();
            assert!(
                known.exists(),
                "an attach to the same endpoint keeps the key"
            );
            // Another workspace's publish does not touch it either.
            ssh.publish(&name("db"), Path::new("/run/ep-9")).unwrap();
            assert!(known.exists());
            ssh.publish(&name("web"), Path::new("/run/ep-2")).unwrap();
            assert!(!known.exists());
            // A host that starts anew has no entries, so its first publish forgets the key.
            std::fs::write(&known, "puddle-web ssh-ed25519 AAAA key\n").unwrap();
            drop(ssh);
            let (again, _) = setup(dir.path(), false);
            again.publish(&name("web"), Path::new("/run/ep-2")).unwrap();
            assert!(!known.exists());
        }

        #[test]
        fn a_config_that_cannot_be_read_is_an_error_not_an_overwrite() {
            let dir = tempfile::tempdir().unwrap();
            let user = dir.path().join("config");
            std::fs::create_dir(&user).unwrap();
            assert!(ensure_include(&user, &dir.path().join("p")).is_err());
        }
    }
}
