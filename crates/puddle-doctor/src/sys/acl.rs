// SPDX-License-Identifier: AGPL-3.0-or-later
//! The extended ACL of a folder, read from `ls -lde` on macOS and parsed. msb's macOS build
//! refuses a folder above its catalog when another account holds an ALLOW entry that can change
//! the folder (add or delete entries, change attributes, take ownership). The doctor reads the
//! same entries from `ls` (names, no unsafe code); a group, or a name it cannot place, counts as
//! another account, which is also msb's side of the doubt.

#[cfg(target_os = "macos")]
use std::io;
#[cfg(target_os = "macos")]
use std::path::Path;

use super::ancestors::Grant;

/// Permissions that let the holder change a folder (msb's `MUTATION_MASK`).
const MUTATING: &[&str] = &[
    "add_file",
    "add_subdirectory",
    "delete",
    "delete_child",
    "writeattr",
    "writeextattr",
    "writesecurity",
    "chown",
    // The names `ls` prints on a file; a folder's ancestors are never files, kept for parsing.
    "write",
    "append",
];

/// Permissions and flags that change nothing.
const HARMLESS: &[&str] = &[
    "list",
    "search",
    "readattr",
    "readextattr",
    "readsecurity",
    "read",
    "execute",
    "file_inherit",
    "directory_inherit",
    "limit_inherit",
    "only_inherit",
];

/// Whom an entry applies to, as `ls` names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Qualifier {
    User(String),
    /// A group (`group:everyone` among them) or anything `ls` shows without a `user:` prefix.
    Other(String),
}

/// One line of the ACL listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AclEntry {
    pub(super) index: u32,
    pub(super) qualifier: Qualifier,
    pub(super) allow: bool,
    pub(super) mutating: bool,
    /// The line as `ls` printed it after the index, for the doctor's message.
    pub(super) text: String,
}

/// Why `ls -lde` output could not be read. It displays as the finding's detail line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum AclParseError {
    /// `ls` printed nothing.
    #[error("ls printed nothing")]
    Empty,
    /// The first line is not a folder's listing line.
    #[error("unexpected first line {line:?}")]
    FirstLine {
        /// The line.
        line: String,
    },
    /// An entry line that does not parse.
    #[error("unreadable ACL line {line:?}")]
    Entry {
        /// The line.
        line: String,
    },
    /// An entry with a permission word this reader does not know.
    #[error("unknown ACL permission {perm:?} in {line:?}")]
    Permission {
        /// The permission word.
        perm: String,
        /// The line.
        line: String,
    },
}

/// The entries in `ls -lde` output for one folder (empty when it has no ACL). An entry line the
/// parser does not understand is an error, never "no grant".
pub(super) fn parse_ls_lde(output: &str) -> Result<Vec<AclEntry>, AclParseError> {
    let mut lines = output.lines();
    let Some(head) = lines.next() else {
        return Err(AclParseError::Empty);
    };
    if head.starts_with(' ') || head.is_empty() {
        return Err(AclParseError::FirstLine {
            line: head.to_owned(),
        });
    }
    lines
        .filter(|l| !l.trim().is_empty())
        .map(parse_entry)
        .collect()
}

fn parse_entry(line: &str) -> Result<AclEntry, AclParseError> {
    let bad = || AclParseError::Entry {
        line: line.to_owned(),
    };
    let (index, rest) = line.trim().split_once(": ").ok_or_else(bad)?;
    let index: u32 = index.parse().map_err(|_| bad())?;
    let mut words = rest.split_whitespace();
    let qualifier = words.next().ok_or_else(bad)?;
    let mut word = words.next().ok_or_else(bad)?;
    if word == "inherited" {
        word = words.next().ok_or_else(bad)?;
    }
    let allow = match word {
        "allow" => true,
        "deny" => false,
        _ => return Err(bad()),
    };
    let perms = words.next().ok_or_else(bad)?;
    if words.next().is_some() {
        return Err(bad());
    }
    let mut mutating = false;
    for perm in perms.split(',') {
        if MUTATING.contains(&perm) {
            mutating = true;
        } else if !HARMLESS.contains(&perm) {
            return Err(AclParseError::Permission {
                perm: perm.to_owned(),
                line: line.to_owned(),
            });
        }
    }
    let qualifier = match qualifier.strip_prefix("user:") {
        Some(name) => Qualifier::User(name.to_owned()),
        None => Qualifier::Other(qualifier.to_owned()),
    };
    Ok(AclEntry {
        index,
        qualifier,
        allow,
        mutating,
        text: rest.to_owned(),
    })
}

/// The entries that give another account something: ALLOW entries that are not this user's
/// (`me`) or root's. DENY entries restrict and never count.
pub(super) fn grants(entries: &[AclEntry], me: &str) -> Vec<Grant> {
    entries
        .iter()
        .filter(|e| {
            e.allow
                && !matches!(&e.qualifier, Qualifier::User(name) if name == me || name == "root")
        })
        .map(|e| Grant {
            index: e.index,
            text: e.text.clone(),
            mutating: e.mutating,
        })
        .collect()
}

/// The login name of the user running this process.
#[cfg(target_os = "macos")]
pub(super) fn current_user() -> io::Result<String> {
    let out = puddle_process::command("/usr/bin/id").arg("-un").output()?;
    if !out.status.success() {
        return Err(io::Error::other("`id -un` failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The grants on `folder`, read with `/bin/ls -lde`.
#[cfg(target_os = "macos")]
pub(super) fn read_grants(folder: &Path, me: &str) -> io::Result<Vec<Grant>> {
    let out = puddle_process::command("/bin/ls")
        .args(["-lde", "--"])
        .arg(folder)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "ls -lde {}: {}",
            folder.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let entries = parse_ls_lde(&text).map_err(io::Error::other)?;
    Ok(grants(&entries, me))
}

#[cfg(test)]
mod tests;
