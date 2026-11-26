// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starting a workspace from a checkout on this computer.
//!
//! Only the checkout's `.git` directory goes to a guest, read-only, and only to the short-lived
//! maintenance sandbox of the create, which has no route out: it clones the repository onto the
//! workspace's volume and is removed again before the workspace's own sandbox boots. Untracked
//! files (an `.env`, local keys, build output) never leave the host.
//!
//! The host never runs git against the checkout. [`LocalCheckout::open`] looks at it as files:
//! the folder's shape and, as text, the `url` of its `origin` remote, which becomes the new
//! repository's address (an SSH remote of a known Git host in its HTTPS form). A checkout whose
//! objects live elsewhere (alternates, a partial clone) is refused: the guest cannot reach them.
//!
//! In the guest every git process of the import, the `upload-pack` that reads the source
//! included, gets its settings from the environment ([`import_env`]): no hook and no file-system
//! monitor runs, no transport but the local one is allowed, no lazy fetch is made, and Git LFS
//! leaves its files as pointers. The clone uses `--no-local` (git's own transport through
//! `upload-pack`, never a copy of the files by path) and `--no-checkout`, so the work tree is
//! written once, after every ref is in place.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_compute::ExecRequest;
use puddle_types::{CheckoutDir, GuestEnv, GuestPath, OriginRefusal};

use crate::WorkspaceError;
use crate::layout::Layout;

/// Where the `n`-th checkout of an import is mounted in the guest: `/puddle/import/<n>/.git`, so
/// git sees an ordinary (empty) checkout at `/puddle/import/<n>`.
///
/// # Errors
///
/// [`WorkspaceError::Layout`] (never: the path is fixed).
pub fn import_mount(n: usize) -> Result<GuestPath, WorkspaceError> {
    GuestPath::new(&format!("/puddle/import/{n}/.git")).map_err(layout)
}

/// The largest `.git/config` Puddle reads for the origin; a larger one has no origin it uses.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// How long the import's clone of one checkout may take (as a clone over the network).
pub(crate) const IMPORT_TIMEOUT: Duration = Duration::from_secs(600);

/// Why a folder is no checkout Puddle can start a workspace from. It displays as the message
/// shown to the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CheckoutRefusal {
    /// The path is relative.
    #[error("{} is not an absolute path", folder.display())]
    NotAbsolute {
        /// The path as given.
        folder: PathBuf,
    },
    /// The folder is missing or can't be resolved.
    #[error("{} can't be opened: {cause}", folder.display())]
    CantOpen {
        /// The path as given.
        folder: PathBuf,
        /// The operating system's words.
        cause: String,
    },
    /// The folder is a bare repository.
    #[error(
        "{} is a bare repository, which has no checkout to start from; use its address instead",
        folder.display()
    )]
    Bare {
        /// The folder.
        folder: PathBuf,
    },
    /// The folder has no `.git`.
    #[error("{} is not a git checkout: it has no .git", folder.display())]
    NoGit {
        /// The folder.
        folder: PathBuf,
    },
    /// The `.git` entry has the wrong shape (a link file, a link, a linked worktree).
    #[error("the checkout {} {why}", folder.display())]
    Shape {
        /// The folder.
        folder: PathBuf,
        /// What is wrong with the `.git` entry.
        why: puddle_compute::CheckoutDirRefusal,
    },
    /// The objects live in another repository.
    #[error(
        "the checkout {} keeps its objects in another repository (alternates), which the clone \
         can't reach; use its address instead",
        folder.display()
    )]
    Alternates {
        /// The folder.
        folder: PathBuf,
    },
    /// The repository is a partial clone.
    #[error(
        "the checkout {} is a partial clone, whose missing objects the clone can't fetch; \
         use its address instead",
        folder.display()
    )]
    PartialClone {
        /// The folder.
        folder: PathBuf,
    },
}

/// A checkout on this computer, as a create names it: its `.git` directory and what its config
/// says about `origin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalCheckout {
    /// The canonical path of the checkout's `.git` directory: what is mounted.
    pub git_dir: PathBuf,
    /// The folder the user picked, canonical (the parent of `git_dir`).
    pub folder: PathBuf,
    /// The HTTPS address of its `origin` remote, when it has one Puddle can use; the reason
    /// otherwise.
    pub origin: Result<String, OriginRefusal>,
}

impl LocalCheckout {
    /// Looks at the folder `folder` (the checkout the user picked, or its `.git` directly).
    ///
    /// # Errors
    ///
    /// A message for the user: the folder is missing, holds no `.git`, is a bare repository, its
    /// `.git` is a worktree's or submodule's link file or not a git directory, or its objects
    /// are kept elsewhere.
    pub fn open(folder: &Path) -> Result<Self, CheckoutRefusal> {
        if !folder.is_absolute() {
            return Err(CheckoutRefusal::NotAbsolute {
                folder: folder.to_path_buf(),
            });
        }
        let picked = dunce::canonicalize(folder).map_err(|e| CheckoutRefusal::CantOpen {
            folder: folder.to_path_buf(),
            cause: e.to_string(),
        })?;
        let git_dir = if picked.file_name().is_some_and(|n| n == ".git") {
            picked.clone()
        } else {
            picked.join(".git")
        };
        let folder = git_dir
            .parent()
            .map_or_else(|| picked.clone(), Path::to_path_buf);
        if std::fs::symlink_metadata(&git_dir).is_err() {
            let bare = ["HEAD", "objects", "refs"]
                .iter()
                .all(|e| folder.join(e).exists());
            return Err(if bare {
                CheckoutRefusal::Bare { folder }
            } else {
                CheckoutRefusal::NoGit { folder }
            });
        }
        puddle_compute::check_checkout_dir(&git_dir).map_err(|why| CheckoutRefusal::Shape {
            folder: folder.clone(),
            why,
        })?;
        if std::fs::symlink_metadata(git_dir.join("objects").join("info").join("alternates"))
            .is_ok()
        {
            return Err(CheckoutRefusal::Alternates { folder });
        }
        let config = read_config(&git_dir.join("config"));
        if config.as_deref().is_ok_and(is_partial_clone) {
            return Err(CheckoutRefusal::PartialClone { folder });
        }
        let origin = config
            .and_then(|text| origin_url(&text))
            .and_then(|url| https_form(&url));
        Ok(Self {
            git_dir,
            folder,
            origin,
        })
    }
}

/// The git config file `config` as text: a plain file (checked on the open file, so a swap for a
/// link or a device in between does not count) of at most [`MAX_CONFIG_BYTES`].
fn layout(e: impl std::fmt::Display) -> WorkspaceError {
    WorkspaceError::Layout {
        reason: e.to_string(),
    }
}

fn read_config(config: &Path) -> Result<String, OriginRefusal> {
    if !std::fs::symlink_metadata(config).is_ok_and(|m| m.is_file()) {
        return Err(OriginRefusal::NoConfigFile);
    }
    let file = std::fs::File::open(config).map_err(|e| OriginRefusal::ConfigUnreadable {
        cause: e.to_string(),
    })?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(OriginRefusal::ConfigNotPlain);
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| OriginRefusal::ConfigUnreadable {
            cause: e.to_string(),
        })?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(OriginRefusal::ConfigTooLarge);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether config text makes the repository a partial clone (objects to be fetched on demand).
fn is_partial_clone(text: &str) -> bool {
    text.lines().any(|line| {
        let key = line.split('=').next().unwrap_or_default().trim();
        key.eq_ignore_ascii_case("partialclone") || key.eq_ignore_ascii_case("promisor")
    })
}

/// The first `url` in the `[remote "origin"]` section of git config text. Text git could read
/// differently from this reader (a line continued with `\`, an include before the origin, an
/// escape other than `\"` and `\\`) is refused rather than guessed at.
fn origin_url(text: &str) -> Result<String, OriginRefusal> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.lines().any(|l| l.trim_end().ends_with('\\')) {
        return Err(OriginRefusal::Unreadable);
    }
    let mut in_origin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(header) = line.strip_prefix('[') {
            let name = header.trim_start().to_ascii_lowercase();
            if name.starts_with("include") {
                return Err(OriginRefusal::Unreadable);
            }
            in_origin = is_origin_header(line);
            continue;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("url") {
            continue;
        }
        return config_value(value.trim()).ok_or(OriginRefusal::Unreadable);
    }
    Err(OriginRefusal::NoOrigin)
}

/// `[remote "origin"]` (section names ignore case, subsection names do not) or the old
/// `[remote.origin]`, with nothing but a comment after it.
fn is_origin_header(line: &str) -> bool {
    let Some(end) = line.find(']') else {
        return false;
    };
    let rest = line[end + 1..].trim_start();
    if !(rest.is_empty() || rest.starts_with('#') || rest.starts_with(';')) {
        return false;
    }
    let inner = line[1..end].trim();
    if let Some((section, sub)) = inner.split_once(char::is_whitespace) {
        section.eq_ignore_ascii_case("remote") && sub.trim() == "\"origin\""
    } else {
        inner.eq_ignore_ascii_case("remote.origin")
    }
}

/// A config value: quotes removed, `\"` and `\\` unescaped, a comment after it dropped. `None`
/// for a value that holds another escape or an unclosed quote.
fn config_value(value: &str) -> Option<String> {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => match chars.next() {
                Some(e @ ('"' | '\\')) => out.push(e),
                _ => return None,
            },
            '#' | ';' if !quoted => break,
            c => out.push(c),
        }
    }
    let out = out.trim().to_owned();
    (!quoted && !out.is_empty()).then_some(out)
}

/// `url` with any user name and password of a `scheme://` address replaced by `***`, for a
/// message.
fn redacted(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
            match rest[..end].rfind('@') {
                Some(at) => format!("{scheme}://***{}", &rest[at..]),
                None => url.to_owned(),
            }
        }
        None => url.to_owned(),
    }
}

/// Splits `rest` (what follows `scheme://`) into its host, without user name, password or port
/// for SSH, and the rest of the address. The authority ends at the first `/`, `?`, `#` or `\`,
/// as it does for curl and git.
fn authority(rest: &str) -> (&str, &str) {
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    (host, tail)
}

/// The HTTPS form of a remote address: an `https://` address without its user name or password,
/// or an SSH remote of GitHub, GitLab, Bitbucket or Azure DevOps written as HTTPS.
///
/// # Errors
///
/// Why the address can't be used, for a remote of any other kind; the message never shows a
/// password the address held.
pub fn https_form(url: &str) -> Result<String, OriginRefusal> {
    let shown = redacted(url);
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(OriginRefusal::ControlCharacters { shown });
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        let (host, tail) = authority(&url["https://".len()..]);
        if host.is_empty() || !tail.starts_with('/') {
            return Err(OriginRefusal::NoRepository { shown });
        }
        return Ok(format!("https://{host}{tail}"));
    }
    let (host, path) = if lower.starts_with("ssh://") {
        let (host, tail) = authority(&url["ssh://".len()..]);
        let host = host.split(':').next().unwrap_or(host);
        (
            host.to_ascii_lowercase(),
            tail.trim_start_matches('/').to_owned(),
        )
    } else if let Some((authority, path)) = scp_like(url) {
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (
            host.to_ascii_lowercase(),
            path.trim_start_matches('/').to_owned(),
        )
    } else {
        return Err(OriginRefusal::NotHttpsOrSsh { shown });
    };
    if path.contains(['?', '#', '\\']) {
        return Err(OriginRefusal::SshUnreadable { shown });
    }
    match host.as_str() {
        "github.com" | "ssh.github.com" => Ok(format!("https://github.com/{path}")),
        "gitlab.com" => Ok(format!("https://gitlab.com/{path}")),
        "bitbucket.org" => Ok(format!("https://bitbucket.org/{path}")),
        "ssh.dev.azure.com" | "vs-ssh.visualstudio.com" => {
            let parts: Option<Vec<&str>> = path.strip_prefix("v3/").map(|p| p.split('/').collect());
            match parts.as_deref() {
                Some([org, project, repo])
                    if [org, project, repo].iter().all(|p| !p.is_empty()) =>
                {
                    // The same address the proxy and the create form read it as.
                    Ok(if host == "ssh.dev.azure.com" {
                        format!("https://dev.azure.com/{org}/{project}/_git/{repo}")
                    } else {
                        format!("https://{org}.visualstudio.com/{project}/_git/{repo}")
                    })
                }
                _ => Err(OriginRefusal::AzureUnreadable { shown }),
            }
        }
        _ => Err(OriginRefusal::UnknownHost { shown }),
    }
}

/// `user@host:path` (no scheme, a `:` before any `/`, not a drive letter) as `(user@host, path)`.
fn scp_like(url: &str) -> Option<(&str, &str)> {
    if url.contains("://") {
        return None;
    }
    let colon = url.find(':')?;
    let slash = url.find('/').unwrap_or(usize::MAX);
    (colon > 1 && colon < slash).then(|| (&url[..colon], &url[colon + 1..]))
}

/// One checkout an import clones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// The canonical `.git` directory on the host.
    pub git_dir: PathBuf,
    /// The checkout's directory on the volume.
    pub directory: CheckoutDir,
    /// The address its `origin` gets in the clone.
    pub url: String,
}

/// The git settings of every git process of the `n`-th import, as environment (`-c` would not
/// reach the `upload-pack` that reads the source: git clears it for that child): no hook, no
/// file-system monitor, no command for alternates' refs (settings from the environment win over
/// the source's own config), no transport but the local one, no automatic gc, the mounted repository
/// (owned by the host's user) one git may read. `GIT_NO_LAZY_FETCH` keeps a source that claims
/// missing objects from fetching them; `GIT_LFS_SKIP_SMUDGE` leaves LFS files as pointers.
///
/// # Errors
///
/// [`WorkspaceError::Layout`] (never: every name and value is fixed or a fixed path).
pub(crate) fn import_env(n: usize) -> Result<GuestEnv, WorkspaceError> {
    let settings = [
        ("core.hooksPath", "/dev/null".to_owned()),
        ("core.fsmonitor", "false".to_owned()),
        ("safe.directory", source(n)),
        ("protocol.allow", "never".to_owned()),
        ("protocol.file.allow", "always".to_owned()),
        ("gc.auto", "0".to_owned()),
        // The source's config could name a command for the refs of its alternates; this one
        // prints none.
        ("core.alternateRefsCommand", "true".to_owned()),
        ("maintenance.auto", "false".to_owned()),
    ];
    let mut env = GuestEnv::new();
    let mut set = |name: &str, value: &str| env.set(name, value).map_err(layout);
    set("GIT_CONFIG_COUNT", &settings.len().to_string())?;
    for (i, (key, value)) in settings.iter().enumerate() {
        set(&format!("GIT_CONFIG_KEY_{i}"), key)?;
        set(&format!("GIT_CONFIG_VALUE_{i}"), value)?;
    }
    set("GIT_NO_LAZY_FETCH", "1")?;
    set("GIT_LFS_SKIP_SMUDGE", "1")?;
    Ok(env)
}

/// The source folder in the guest of the `n`-th checkout (the parent of [`import_mount`]).
fn source(n: usize) -> String {
    format!("/puddle/import/{n}")
}

/// After the clone, in the clone (`$1`), from the source (`$2`) with `origin`'s address `$3`:
/// every branch and tag of the checkout, and what it knew of `origin`'s branches (not its
/// `origin/HEAD`), become the clone's own; the source remote goes; `origin` is the address; the
/// branch that is checked out follows its namesake on `origin` when the checkout knew one
/// (written as config, so no branch name is read as an option); then the work tree is written.
pub const FINISH_IMPORT_SH: &str = r#"set -eu
dest=$1; src=$2; url=$3
cd "$dest"
git fetch --quiet --update-head-ok "$src" '+refs/heads/*:refs/heads/*' '+refs/tags/*:refs/tags/*' '+refs/remotes/origin/*:refs/remotes/origin/*'
git update-ref -d refs/remotes/origin/HEAD 2>/dev/null || true
git remote remove puddle-checkout
git remote add origin "$url"
if full=$(git symbolic-ref --quiet HEAD); then
  branch=${full#refs/heads/}
  if git rev-parse --quiet --verify "refs/remotes/origin/$branch" >/dev/null; then
    git config "branch.$branch.remote" origin
    git config "branch.$branch.merge" "$full"
  fi
fi
git reset --quiet --hard
"#;

impl Layout {
    /// The clone of the `n`-th import: `git clone --no-local --no-checkout --origin
    /// puddle-checkout -- <source> <checkout>`, with `import_env`; an argument vector, never a
    /// shell string.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout).
    pub fn import_request(
        &self,
        n: usize,
        dir: &CheckoutDir,
    ) -> Result<ExecRequest, WorkspaceError> {
        let dest = self.checkout(dir)?;
        let src = source(n);
        let args = [
            "clone",
            "--quiet",
            "--no-local",
            "--no-checkout",
            "--origin",
            "puddle-checkout",
            "--",
            &src,
            dest.as_str(),
        ];
        Ok(ExecRequest::new("git", args.map(str::to_owned))
            .with_env(&import_env(n)?)
            .with_timeout(IMPORT_TIMEOUT))
    }

    /// The step after [`Layout::import_request`]: [`FINISH_IMPORT_SH`] with the clone, the
    /// source and `origin`'s address as its arguments, and `import_env`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout).
    pub fn finish_import_request(
        &self,
        n: usize,
        dir: &CheckoutDir,
        url: &str,
    ) -> Result<ExecRequest, WorkspaceError> {
        let dest = self.checkout(dir)?;
        let args = [
            "-c",
            FINISH_IMPORT_SH,
            "puddle-import",
            dest.as_str(),
            &source(n),
            url,
        ];
        Ok(ExecRequest::new("sh", args.map(str::to_owned))
            .with_env(&import_env(n)?)
            .with_timeout(IMPORT_TIMEOUT))
    }
}

#[cfg(test)]
mod tests;
