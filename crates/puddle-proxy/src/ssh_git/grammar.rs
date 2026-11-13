// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a guest may ask a terminated Git host over SSH, and which repository it names.
//!
//! The command is read with one strict grammar: `git-upload-pack` or
//! `git-receive-pack`, one space, one argument in single quotes. The argument is the host's own
//! path form, `owner/repo[.git]` (also with a leading `/`, and with more groups on a host that
//! nests them), or `v3/org/project/repo` on Azure DevOps. Only the bytes a repository name is
//! made of are read, so what comes out is a canonical HTTPS path that the table's own reading (the
//! injector's, the one HTTPS requests get) takes at face value. Anything else is refused here and
//! nothing goes upstream.
//!
//! The two Git LFS commands a client tries before its HTTPS address are answered by the caller:
//! `git-lfs-transfer` as a host without it, `git-lfs-authenticate` with the HTTPS address.

use puddle_types::Host;

/// Which of Git's two services a command asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitService {
    /// `git-upload-pack`: a fetch or clone.
    Upload,
    /// `git-receive-pack`: a push.
    Receive,
}

impl GitService {
    /// The command's name, as the audit records it in the method field.
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Upload => "git-upload-pack",
            Self::Receive => "git-receive-pack",
        }
    }

    /// The `service=` value of the smart-HTTP advertisement.
    #[must_use]
    pub fn http_service(self) -> &'static str {
        self.command()
    }
}

/// A Git host Puddle ends SSH for, and the HTTPS side it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitHost {
    /// The HTTPS host the injector and the table know it by (`github.com` for `ssh.github.com`).
    /// For Azure DevOps' old `vs-ssh` name it depends on the organisation in the path.
    pub(crate) https: Host,
    pub(crate) form: PathForm,
}

/// How a host spells a repository in an SSH command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathForm {
    /// `owner/repo[.git]`, as GitHub, GitLab, Bitbucket and an enterprise server have it.
    Plain,
    /// `v3/org/project/repo` on `ssh.dev.azure.com`.
    AzureDevOps,
    /// `v3/org/project/repo` on `vs-ssh.visualstudio.com` (the HTTPS host is `org.visualstudio.com`).
    VisualStudio,
}

/// The built-in Git hosts: the SSH name, the HTTPS name and the path form.
const BUILT_IN: [(&str, &str, PathForm); 8] = [
    ("github.com", "github.com", PathForm::Plain),
    ("ssh.github.com", "github.com", PathForm::Plain),
    ("gitlab.com", "gitlab.com", PathForm::Plain),
    ("altssh.gitlab.com", "gitlab.com", PathForm::Plain),
    ("bitbucket.org", "bitbucket.org", PathForm::Plain),
    ("altssh.bitbucket.org", "bitbucket.org", PathForm::Plain),
    ("ssh.dev.azure.com", "dev.azure.com", PathForm::AzureDevOps),
    (
        "vs-ssh.visualstudio.com",
        "visualstudio.com",
        PathForm::VisualStudio,
    ),
];

/// The SSH names of the built-in Git hosts, the ones every workspace has.
pub(crate) fn built_in_names() -> Vec<Host> {
    BUILT_IN
        .iter()
        .filter_map(|(ssh, ..)| Host::parse_normalised(ssh).ok())
        .collect()
}

/// The built-in Git host called `name`, if there is one.
pub(crate) fn built_in(name: &Host) -> Option<GitHost> {
    let name = name.to_string();
    BUILT_IN
        .iter()
        .find(|(ssh, ..)| *ssh == name)
        .and_then(|(_, https, form)| {
            Some(GitHost {
                https: Host::parse_normalised(https).ok()?,
                form: *form,
            })
        })
}

/// A repository, as the guest named it, read into the HTTPS spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Repository {
    /// The HTTPS host to ask the table about.
    pub(crate) host: Host,
    /// The repository's HTTPS path, canonical and without a trailing endpoint: `/acme/web.git`,
    /// `/org/project/_git/repo`.
    pub(crate) path: String,
}

impl Repository {
    /// `owner/repo` in the table's one spelling (lower case, no `.git`, no `_git`), for the audit
    /// record's path field.
    pub(crate) fn audit_path(&self) -> String {
        let lower = self.path.to_ascii_lowercase();
        let trimmed = lower.strip_suffix(".git").unwrap_or(&lower);
        trimmed.replace("/_git/", "/")
    }
}

/// A command the grammar accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Command {
    /// A fetch or a push of `repository`.
    Git(GitService, Repository),
    /// `git-lfs-authenticate <repository> download|upload`: answered with the HTTPS address.
    LfsAuthenticate {
        repository: Repository,
        upload: bool,
    },
    /// `git-lfs-transfer`: answered as a host without it.
    LfsTransfer,
}

/// Why a command is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Not one of the commands above (`ssh_command_unsupported`).
    Unsupported,
    /// The repository path is not one Puddle reads (`bad_git_path`).
    BadPath,
}

/// Reads `command` for a host that spells paths as `host.form`.
pub(crate) fn parse(command: &str, host: &GitHost) -> Result<Command, Refused> {
    if command == "git-lfs-transfer" || command.starts_with("git-lfs-transfer ") {
        return Ok(Command::LfsTransfer);
    }
    let (word, rest) = command.split_once(' ').ok_or(Refused::Unsupported)?;
    let service = match word {
        "git-upload-pack" => Some(GitService::Upload),
        "git-receive-pack" => Some(GitService::Receive),
        "git-lfs-authenticate" => None,
        _ => return Err(Refused::Unsupported),
    };
    if let Some(service) = service {
        if rest.contains(' ') {
            return Err(Refused::Unsupported);
        }
        return Ok(Command::Git(service, repository(rest, host)?));
    }
    let (argument, operation) = rest.split_once(' ').ok_or(Refused::Unsupported)?;
    let upload = match operation {
        "upload" => true,
        "download" => false,
        _ => return Err(Refused::Unsupported),
    };
    Ok(Command::LfsAuthenticate {
        repository: repository(argument, host)?,
        upload,
    })
}

/// The argument, unquoted and read into an HTTPS path.
fn repository(argument: &str, host: &GitHost) -> Result<Repository, Refused> {
    let inner = argument
        .strip_prefix('\'')
        .and_then(|a| a.strip_suffix('\''))
        .ok_or(Refused::BadPath)?;
    let inner = inner.strip_prefix('/').unwrap_or(inner);
    let segments: Vec<&str> = inner.split('/').collect();
    match host.form {
        PathForm::Plain => plain(&segments, host),
        PathForm::AzureDevOps | PathForm::VisualStudio => azure(&segments, host),
    }
}

fn plain(segments: &[&str], host: &GitHost) -> Result<Repository, Refused> {
    if segments.len() < 2 || !segments.iter().all(|s| name_ok(s, false)) {
        return Err(Refused::BadPath);
    }
    let (last, owners) = segments.split_last().ok_or(Refused::BadPath)?;
    let repo = last.strip_suffix(".git").unwrap_or(last);
    if !name_ok(repo, false) || repo.strip_suffix(".git").is_some() {
        return Err(Refused::BadPath);
    }
    Ok(Repository {
        host: host.https.clone(),
        path: format!("/{}/{repo}.git", owners.join("/")),
    })
}

fn azure(segments: &[&str], host: &GitHost) -> Result<Repository, Refused> {
    let [version, org, project, repo] = segments else {
        return Err(Refused::BadPath);
    };
    if *version != "v3" || !name_ok(org, false) || !name_ok(project, true) || !name_ok(repo, true) {
        return Err(Refused::BadPath);
    }
    if host.form == PathForm::AzureDevOps {
        Ok(Repository {
            host: host.https.clone(),
            path: format!("/{org}/{project}/_git/{repo}"),
        })
    } else {
        let https = Host::parse_normalised(&format!("{org}.visualstudio.com"))
            .map_err(|_| Refused::BadPath)?;
        Ok(Repository {
            host: https,
            path: format!("/{project}/_git/{repo}"),
        })
    }
}

/// Whether `name` is one path segment made of the bytes a repository, owner, organisation or
/// project name has: letters, digits, `-`, `_`, `.`, and (`spaced`, Azure DevOps) `%20` for a
/// space. Not empty, not `.` or `..`.
fn name_ok(name: &str, spaced: bool) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    let mut rest = name;
    while let Some(c) = rest.chars().next() {
        if spaced && rest.starts_with("%20") {
            rest = rest.get(3..).unwrap_or_default();
            continue;
        }
        if !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return false;
        }
        rest = rest.get(c.len_utf8()..).unwrap_or_default();
    }
    true
}

#[cfg(test)]
mod tests;
