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

use puddle_types::{SessionUser, WorkspaceId};

use crate::ssh_login::{LoginKey, SSH_DIR};

const CONFIG_FILE: &str = "config";
const KNOWN_HOSTS_DIR: &str = "known_hosts";

/// Why Puddle's ssh config could not be made, written or linked. The messages are for the user.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SshConfigError {
    /// The login key could not be made. Shared (`Arc`) because it is kept from start and said each
    /// time it matters.
    #[error("cannot make the login key in {}: {source}", dir.display())]
    LoginKey {
        /// Where the key lives.
        dir: PathBuf,
        /// What went wrong.
        source: std::sync::Arc<io::Error>,
    },
    /// The login key file could not be read.
    #[error("the login key cannot be read: {0}")]
    LoginKeyUnreadable(#[source] russh::keys::Error),
    /// No `puddle` program to run as the bridge.
    #[error("this Puddle has no `puddle` program to run as the ssh bridge")]
    NoBridge,
    /// The old host key could not be removed.
    #[error("cannot forget the old host key: {0}")]
    ForgetHostKey(#[source] io::Error),
    /// Puddle's config file could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// An entry could not be taken out of the file.
    #[error("cannot take {host} out of {}: {source}", path.display())]
    Withdraw {
        /// The entry's host name.
        host: String,
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: Box<SshConfigError>,
    },
    /// The include line could not be added to the user's file.
    #[error("cannot add Puddle's include line to {}: {source}", path.display())]
    Include {
        /// The user's file.
        path: PathBuf,
        /// What went wrong.
        source: Box<SshConfigError>,
    },
    /// A value an ssh config cannot hold safely.
    #[error("{value:?} has the character {character:?}, which an ssh config cannot hold safely")]
    Unsafe {
        /// The value.
        value: String,
        /// The character that makes it unsafe.
        character: char,
    },
    /// A path that is not text.
    #[error("{} is not valid UTF-8", .0.display())]
    NotUtf8(PathBuf),
    /// The user's config is a link to nothing.
    #[error(
        "{} is a symbolic link to a file that is not there, and Puddle will not replace the link",
        .0.display()
    )]
    DanglingLink(PathBuf),
    /// The user's config is read-only.
    #[error("{} is read-only, and Puddle will not change it", .0.display())]
    ReadOnly(PathBuf),
    /// A file-system call failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One workspace's entry: the endpoint `ssh` reaches it through and the guest user it logs in as.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    endpoint: PathBuf,
    user: SessionUser,
}

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
pub fn ssh_host(workspace: &WorkspaceId) -> String {
    format!("puddle-{workspace}")
}

/// What the ssh config needs and the entries that are live now.
#[derive(Debug)]
pub(crate) struct SshSetup {
    dir: PathBuf,
    login: Result<LoginKey, std::sync::Arc<io::Error>>,
    bridge: Option<PathBuf>,
    user_config: Option<PathBuf>,
    entries: Mutex<BTreeMap<WorkspaceId, Entry>>,
}

impl SshSetup {
    /// The setup under `data`: the login key (made if needed) and an emptied config file, since
    /// no endpoint is open yet. A failure to make the key is kept and said when it matters.
    pub(crate) fn open(data: &Path, settings: &SshSettings) -> Self {
        let dir = data.join(SSH_DIR);
        let login = puddle_fs::private::create_dir(&dir)
            .and_then(|()| LoginKey::open(&dir))
            .map_err(std::sync::Arc::new);
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

    /// The login key, or why there is none.
    fn login(&self) -> Result<&LoginKey, SshConfigError> {
        self.login
            .as_ref()
            .map_err(|source| SshConfigError::LoginKey {
                dir: self.dir.clone(),
                source: std::sync::Arc::clone(source),
            })
    }

    /// The login key as the host's own SSH client signs in with it (the terminal). The private
    /// file is read into memory for the session and never logged or shown.
    pub(crate) fn identity(&self) -> Result<crate::terminals::Identity, SshConfigError> {
        crate::terminals::Identity::load(self.login()?.private_path())
            .map_err(SshConfigError::LoginKeyUnreadable)
    }

    /// Puddle's own ssh config file.
    pub(crate) fn config_path(&self) -> PathBuf {
        self.dir.join(CONFIG_FILE)
    }

    fn known_hosts(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join(KNOWN_HOSTS_DIR).join(workspace.as_str())
    }

    /// Makes `workspace`'s entry name `endpoint` (the caller has checked the direct SSH gate),
    /// and the user's ssh config include Puddle's file. The workspace's host key is forgotten
    /// when the endpoint is a new one, and kept when it is the endpoint the entry already names.
    ///
    /// # Errors
    ///
    /// Why the entry could not be written, for the user.
    pub(crate) fn publish(
        &self,
        workspace: &WorkspaceId,
        endpoint: &Path,
        user: SessionUser,
    ) -> Result<(), SshConfigError> {
        let login = self.login()?;
        let bridge = self.bridge.as_deref().ok_or(SshConfigError::NoBridge)?;
        let known_hosts = self.known_hosts(workspace);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let before = entries.insert(
            workspace.clone(),
            Entry {
                endpoint: endpoint.to_path_buf(),
                user,
            },
        );
        let context = Context {
            bridge,
            identity: login.private_path(),
            known_hosts_dir: &self.dir.join(KNOWN_HOSTS_DIR),
        };
        // A sandbox made anew has a new endpoint and a new host key, and a key left over would stop
        // `ssh` from connecting. The same endpoint is the same sandbox: its key stays, so every
        // later attach is checked against it.
        let endpoint_changed = before.as_ref().map(|e| e.endpoint.as_path()) != Some(endpoint);
        let written = render(&entries, &context).and_then(|text| {
            if endpoint_changed {
                match std::fs::remove_file(&known_hosts) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(SshConfigError::ForgetHostKey(e)),
                }
            }
            puddle_fs::private::create_dir(context.known_hosts_dir)
                .and_then(|()| {
                    puddle_fs::private::write_atomic(&self.config_path(), text.as_bytes())
                })
                .map_err(|source| SshConfigError::Write {
                    path: self.config_path(),
                    source,
                })
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
    ///
    /// # Errors
    ///
    /// Why the file could not be rewritten: the entry is then still in it, naming an endpoint that
    /// is about to close, and the caller says so. The entry is out of the live set either way, so
    /// the next write of the file (any workspace's) drops it.
    pub(crate) fn withdraw(&self, workspace: &WorkspaceId) -> Result<(), SshConfigError> {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.remove(workspace).is_none() {
            return Ok(());
        }
        if let Err(e) = self.write_config(&entries) {
            return Err(SshConfigError::Withdraw {
                host: ssh_host(workspace),
                path: self.config_path(),
                source: Box::new(e),
            });
        }
        let _ = std::fs::remove_file(self.known_hosts(workspace));
        Ok(())
    }

    /// The endpoint `workspace`'s entry names now.
    #[cfg(all(test, unix))]
    pub(crate) fn endpoint(&self, workspace: &WorkspaceId) -> Option<PathBuf> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .map(|e| e.endpoint.clone())
    }

    fn write_config(&self, entries: &BTreeMap<WorkspaceId, Entry>) -> Result<(), SshConfigError> {
        let (Ok(login), Some(bridge)) = (&self.login, &self.bridge) else {
            return Ok(puddle_fs::private::write_atomic(
                &self.config_path(),
                HEADER.as_bytes(),
            )?);
        };
        let context = Context {
            bridge,
            identity: login.private_path(),
            known_hosts_dir: &self.dir.join(KNOWN_HOSTS_DIR),
        };
        let text = render(entries, &context)?;
        Ok(puddle_fs::private::write_atomic(
            &self.config_path(),
            text.as_bytes(),
        )?)
    }

    fn include(&self) -> Result<(), SshConfigError> {
        let Some(user) = &self.user_config else {
            return Ok(());
        };
        ensure_include(user, &self.config_path()).map_err(|source| SshConfigError::Include {
            path: user.clone(),
            source: Box::new(source),
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
    entries: &BTreeMap<WorkspaceId, Entry>,
    context: &Context<'_>,
) -> Result<String, SshConfigError> {
    let mut text = HEADER.to_owned();
    for (workspace, Entry { endpoint, user }) in entries {
        let host = ssh_host(workspace);
        let identity = quoted(context.identity)?;
        let known_hosts = quoted(&context.known_hosts_dir.join(workspace.as_str()))?;
        let proxy = proxy_command(context.bridge, endpoint)?;
        let _ = write!(
            text,
            "\nHost {host}\n\
             \x20   HostName {host}\n\
             \x20   User {user}\n\
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
fn checked(value: &str) -> Result<&str, SshConfigError> {
    let forbidden = |c: char| {
        c.is_control() || matches!(c, '"' | '%' | '$' | '`') || (!cfg!(windows) && c == '\\')
    };
    match value.chars().find(|&c| forbidden(c)) {
        None => Ok(value),
        Some(character) => Err(SshConfigError::Unsafe {
            value: value.to_owned(),
            character,
        }),
    }
}

/// `path` as a double-quoted ssh config word, with forward slashes on Windows (OpenSSH reads a
/// backslash as an escape in some places).
fn quoted(path: &Path) -> Result<String, SshConfigError> {
    let text = path
        .to_str()
        .ok_or_else(|| SshConfigError::NotUtf8(path.to_owned()))?;
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text.to_owned()
    };
    Ok(format!("\"{}\"", checked(&text)?))
}

/// `"<bridge>" ssh-bridge "<endpoint>"`: the rest of the line is the command, which the shell
/// (or `cmd` on Windows) reads, so the endpoint keeps its backslashes (`\\.\pipe\...`).
fn proxy_command(bridge: &Path, endpoint: &Path) -> Result<String, SshConfigError> {
    let word = |p: &Path| -> Result<String, SshConfigError> {
        let text = p
            .to_str()
            .ok_or_else(|| SshConfigError::NotUtf8(p.to_owned()))?;
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
pub(crate) fn ensure_include(user: &Path, puddle: &Path) -> Result<(), SshConfigError> {
    let line = format!("Include {}", quoted(puddle)?);
    let target = match std::fs::canonicalize(user) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let is_link = std::fs::symlink_metadata(user).is_ok_and(|m| m.is_symlink());
            if is_link {
                return Err(SshConfigError::DanglingLink(user.to_path_buf()));
            }
            user.to_path_buf()
        }
        Err(e) => return Err(e.into()),
    };
    let (existing, seen) = match std::fs::read(&target) {
        Ok(bytes) => {
            let meta = std::fs::metadata(&target)?;
            (Some(bytes), Some(meta))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (None, None),
        Err(e) => return Err(e.into()),
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
        return Ok(puddle_fs::private::write_atomic(&target, &new)?);
    };
    if seen.permissions().readonly() {
        return Err(SshConfigError::ReadOnly(target));
    }
    Ok(replace_keeping_permissions(&target, &new, &seen)?)
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
    getrandom::fill(&mut suffix).map_err(io::Error::other)?;
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
mod tests;
