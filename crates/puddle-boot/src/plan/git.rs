// SPDX-License-Identifier: AGPL-3.0-or-later
//! The git identity and author rules of a plan, and the git config files they become.

use super::*;

/// The git identity Puddle writes into the sandbox (a basic identities feature: the user
/// sets it in Puddle, nothing is copied from the host's `.gitconfig`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    name: String,
    email: String,
}

impl GitIdentity {
    /// Checks and wraps `user.name` and `user.email`.
    ///
    /// # Errors
    ///
    /// [`PlanError::GitIdentity`] when either is empty or contains a control character.
    pub fn new(name: &str, email: &str) -> Result<Self, PlanError> {
        for (field, value) in [("user.name", name), ("user.email", email)] {
            if value.trim().is_empty() {
                return Err(PlanError::GitIdentity {
                    field,
                    reason: "is empty",
                });
            }
            if value.chars().any(char::is_control) {
                return Err(PlanError::GitIdentity {
                    field,
                    reason: "contains a control character",
                });
            }
        }
        Ok(Self {
            name: name.to_owned(),
            email: email.to_owned(),
        })
    }

    /// `user.name`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `user.email`.
    #[must_use]
    pub fn email(&self) -> &str {
        &self.email
    }
}

/// An author for the repositories that have a remote matching one of `globs`: git's
/// `includeIf "hasconfig:remote.*.url:<glob>"`. The match is on any remote of the repository, is
/// case-sensitive, and `pushurl` is ignored.
///
/// Rules go into the plan in git's order of precedence: when a repository matches several, the
/// one written **last** wins, whatever the order of its remotes. The caller orders them, lowest
/// priority first. The author is a convenience for the user's commits, not a security control:
/// the guest can set any `user.email` in a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitAuthorRule {
    globs: Vec<String>,
    author: GitIdentity,
}

impl GitAuthorRule {
    /// Checks and wraps a rule.
    ///
    /// # Errors
    ///
    /// [`PlanError::GitAuthorRule`] when there is no glob, too many, or one is empty, too long or
    /// holds a control character, a quote or a backslash (nothing a remote glob needs).
    pub fn new(globs: Vec<String>, author: GitIdentity) -> Result<Self, PlanError> {
        let bad = |reason| Err(PlanError::GitAuthorRule { reason });
        if globs.is_empty() {
            return bad("has no remote glob");
        }
        if globs.len() > MAX_GIT_AUTHOR_GLOBS {
            return bad("has too many remote globs");
        }
        for glob in &globs {
            if glob.is_empty() || glob.len() > 1024 {
                return bad("has an empty or over-long remote glob");
            }
            if glob
                .chars()
                .any(|c| c.is_control() || c == '"' || c == '\\')
            {
                return bad("has a remote glob with a control character, quote or backslash");
            }
        }
        Ok(Self { globs, author })
    }

    /// The remote globs.
    #[must_use]
    pub fn globs(&self) -> &[String] {
        &self.globs
    }

    /// The author.
    #[must_use]
    pub fn author(&self) -> &GitIdentity {
        &self.author
    }
}

/// A quoted git config value.
fn git_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A `[user]` section.
fn git_user(id: &GitIdentity) -> String {
    format!(
        "[user]\n\tname = {}\n\temail = {}\n",
        git_quote(&id.name),
        git_quote(&id.email)
    )
}

/// The file name of an author, relative to [`GIT_CONFIG_GUEST`]'s directory. The name follows the
/// author's text, so a file never changes meaning: the guest takes a plan's files one at a time
/// while git may read them, and a name that stood for another author after an update (rule
/// number 1 first Work, then Personal) made a remote get the wrong author between two writes.
pub(super) fn git_author_name(author: &GitIdentity) -> String {
    let hash = git_user(author)
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("git-author-{hash:016x}.gitconfig")
}

/// Puddle's git settings: `core.fsync=added` and `core.fsyncMethod=batch` always (ADR 0006: no
/// lost commits on a VM kill or a power cut, and the index is synced too), the identity when set, then one `includeIf` per remote glob of each author rule, in
/// order (git lets the last match win), each pointing at that rule's author file. Never a
/// credential helper. With a session user other than root, `safe.directory = *`: the checkouts
/// belong to that user, and a root shell (`sudo -i`) would otherwise find every one refused as
/// "dubious ownership". The check guards one user from another's repository, and in a guest
/// whose only user becomes root at will there is no other user to guard.
pub(super) fn git_config(plan: &BootPlanBuilder) -> GuestFile {
    let mut text = String::from(GENERATED);
    text.push_str("[core]\n\tfsync = added\n\tfsyncMethod = batch\n");
    if !plan.session_user.is_root() {
        text.push_str("[safe]\n\tdirectory = *\n");
    }
    if let Some(id) = &plan.git {
        text.push_str(&git_user(id));
    }
    for rule in &plan.git_rules {
        for glob in &rule.globs {
            text.extend([
                "[includeIf \"hasconfig:remote.*.url:",
                glob,
                "\"]\n\tpath = ",
                &git_author_name(&rule.author),
                "\n",
            ]);
        }
    }
    GuestFile::new(guest_path!(GIT_CONFIG_GUEST), text.into_bytes())
}

/// The author file of each rule: only a `[user]` section (an included file may not define
/// remotes, and needs nothing else).
pub(super) fn git_author_files(rules: &[GitAuthorRule]) -> Result<Vec<GuestFile>, PlanError> {
    let mut seen = BTreeSet::new();
    rules
        .iter()
        .filter(|rule| seen.insert(git_author_name(&rule.author)))
        .map(|rule| {
            let path = format!("{GIT_AUTHOR_DIR_GUEST}/{}", git_author_name(&rule.author));
            let path = GuestPath::new(&path).map_err(|_| PlanError::UnsafePath { path })?;
            Ok(GuestFile::new(
                path,
                format!("{GENERATED}{}", git_user(&rule.author)).into_bytes(),
            ))
        })
        .collect()
}
