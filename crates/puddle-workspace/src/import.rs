// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_types::{CheckoutDir, GuestEnv, GuestPath};

use crate::WorkspaceError;
use crate::layout::Layout;

/// Where the `n`-th checkout of an import is mounted in the guest: `/puddle/import/<n>/.git`, so
/// git sees an ordinary (empty) checkout at `/puddle/import/<n>`.
///
/// # Errors
///
/// [`WorkspaceError::Layout`] (never: the path is fixed).
pub fn import_mount(n: usize) -> Result<GuestPath, WorkspaceError> {
    GuestPath::new(&format!("/puddle/import/{n}/.git")).map_err(|e| WorkspaceError::Layout {
        reason: e.to_string(),
    })
}

/// The largest `.git/config` Puddle reads for the origin; a larger one has no origin it uses.
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

/// How long the import's clone of one checkout may take (as a clone over the network).
pub(crate) const IMPORT_TIMEOUT: Duration = Duration::from_secs(600);

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
    pub origin: Result<String, String>,
}

impl LocalCheckout {
    /// Looks at the folder `folder` (the checkout the user picked, or its `.git` directly).
    ///
    /// # Errors
    ///
    /// A message for the user: the folder is missing, holds no `.git`, is a bare repository, its
    /// `.git` is a worktree's or submodule's link file or not a git directory, or its objects
    /// are kept elsewhere.
    pub fn open(folder: &Path) -> Result<Self, String> {
        if !folder.is_absolute() {
            return Err(format!("{} is not an absolute path", folder.display()));
        }
        let picked = dunce::canonicalize(folder)
            .map_err(|e| format!("{} can't be opened: {e}", folder.display()))?;
        let git_dir = if picked.file_name().is_some_and(|n| n == ".git") {
            picked.clone()
        } else {
            picked.join(".git")
        };
        let folder = git_dir
            .parent()
            .map_or_else(|| picked.clone(), Path::to_path_buf);
        let refuse = |why: &str| format!("the checkout {} {why}", folder.display());
        if std::fs::symlink_metadata(&git_dir).is_err() {
            let bare = ["HEAD", "objects", "refs"]
                .iter()
                .all(|e| folder.join(e).exists());
            return Err(if bare {
                format!(
                    "{} is a bare repository, which has no checkout to start from; \
                     use its address instead",
                    folder.display()
                )
            } else {
                format!("{} is not a git checkout: it has no .git", folder.display())
            });
        }
        puddle_compute::check_checkout_dir(&git_dir).map_err(|why| refuse(&why))?;
        if std::fs::symlink_metadata(git_dir.join("objects").join("info").join("alternates"))
            .is_ok()
        {
            return Err(refuse(
                "keeps its objects in another repository (alternates), which the clone can't \
                 reach; use its address instead",
            ));
        }
        let config = read_config(&git_dir.join("config"));
        if config.as_deref().is_ok_and(is_partial_clone) {
            return Err(refuse(
                "is a partial clone, whose missing objects the clone can't fetch; \
                 use its address instead",
            ));
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
fn read_config(config: &Path) -> Result<String, String> {
    if !std::fs::symlink_metadata(config).is_ok_and(|m| m.is_file()) {
        return Err("it has no config file, so no origin remote".into());
    }
    let file = std::fs::File::open(config).map_err(|e| format!("its config can't be read: {e}"))?;
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err("its config is not a plain file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("its config can't be read: {e}"))?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("its config file is too large to read".into());
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

/// The reason given for an origin this reader could read differently from git.
const UNREADABLE: &str = "its origin is written in a way Puddle does not read (a continued line, \
                          an include before it, or an escape); give the repository's HTTPS address";

/// The first `url` in the `[remote "origin"]` section of git config text. Text git could read
/// differently from this reader (a line continued with `\`, an include before the origin, an
/// escape other than `\"` and `\\`) is refused rather than guessed at.
fn origin_url(text: &str) -> Result<String, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.lines().any(|l| l.trim_end().ends_with('\\')) {
        return Err(UNREADABLE.into());
    }
    let mut in_origin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(header) = line.strip_prefix('[') {
            let name = header.trim_start().to_ascii_lowercase();
            if name.starts_with("include") {
                return Err(UNREADABLE.into());
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
        return config_value(value.trim()).ok_or_else(|| UNREADABLE.into());
    }
    Err("it has no origin remote".into())
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
pub fn https_form(url: &str) -> Result<String, String> {
    let shown = redacted(url);
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "its origin {shown} holds spaces or control characters; give the repository's HTTPS address"
        ));
    }
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        let (host, tail) = authority(&url["https://".len()..]);
        if host.is_empty() || !tail.starts_with('/') {
            return Err(format!(
                "its origin {shown} names no repository on a host; give the repository's HTTPS address"
            ));
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
        return Err(format!(
            "its origin {shown} is not an HTTPS or SSH address; give the repository's HTTPS address"
        ));
    };
    if path.contains(['?', '#', '\\']) {
        return Err(format!(
            "its origin {shown} is an SSH address Puddle can't read; give the repository's HTTPS address"
        ));
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
                    Ok(format!("https://dev.azure.com/{org}/{project}/_git/{repo}"))
                }
                _ => Err(format!(
                    "its origin {shown} is an Azure DevOps SSH address Puddle can't read; \
                     give the repository's HTTPS address"
                )),
            }
        }
        _ => Err(format!(
            "its origin {shown} is an SSH address of a host Puddle knows no HTTPS form for; \
             give the repository's HTTPS address"
        )),
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
    let mut set = |name: &str, value: &str| {
        env.set(name, value).map_err(|e| WorkspaceError::Layout {
            reason: e.to_string(),
        })
    };
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
mod tests {
    use puddle_types::WorkspaceId;

    use super::*;

    #[test]
    fn the_origin_is_the_first_url_of_its_section_in_any_spelling() {
        let text = "\u{feff}[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.org/up.git\n\
                    [Remote \"origin\"]  # mine\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                    \tURL = \"https://github.com/acme/api.git\" ; comment\n\turl = https://second\n";
        assert_eq!(
            origin_url(text).as_deref(),
            Ok("https://github.com/acme/api.git")
        );
        assert_eq!(
            origin_url("[remote.origin]\nurl=git@github.com:acme/api.git\n").as_deref(),
            Ok("git@github.com:acme/api.git")
        );
        let none = |text: &str| origin_url(text).unwrap_err();
        // The subsection name is exact; nothing else counts.
        assert_eq!(
            none("[remote \"Origin\"]\nurl = https://x/y\n"),
            "it has no origin remote"
        );
        assert_eq!(
            none("[remote \"origin\"] junk\nurl = https://x/y\n"),
            "it has no origin remote"
        );
        assert_eq!(
            none("[remote \"origin\"]\nfetch = x\n[core]\nurl = https://x/y\n"),
            "it has no origin remote"
        );
        assert_eq!(none("url = https://x/y\n"), "it has no origin remote");
        assert_eq!(none(""), "it has no origin remote");
    }

    #[test]
    fn text_git_could_read_differently_is_refused_not_guessed_at() {
        for text in [
            // A continued line: git would read `url = ...` as part of `fetch`.
            "[remote \"origin\"]\n\tfetch = x \\\n\turl = https://evil.example/x.git\n",
            // A continued line in another section that swallows the header.
            "[core]\n\tfoo = bar\\\n[remote \"origin\"]\n\turl = https://evil.example/x.git\n",
            // An include before the origin may set it first.
            "[include]\n\tpath = other\n[remote \"origin\"]\n\turl = https://x/y\n",
            "[includeIf \"gitdir:~/\"]\n\tpath = other\n[remote \"origin\"]\n\turl = https://x/y\n",
            // Escapes this reader does not take.
            "[remote \"origin\"]\n\turl = https://x/\\ty\n",
            "[remote \"origin\"]\n\turl = \"unclosed\n",
        ] {
            assert_eq!(origin_url(text), Err(UNREADABLE.to_owned()), "{text}");
        }
        assert_eq!(config_value(r#""a\"b\\c""#).as_deref(), Some(r#"a"b\c"#));
        assert_eq!(
            config_value(r#""a # not a comment""#).as_deref(),
            Some("a # not a comment")
        );
        assert_eq!(config_value("  # only a comment"), None);
    }

    #[test]
    fn ssh_remotes_of_known_hosts_get_their_https_form_and_credentials_go() {
        for (remote, https) in [
            (
                "https://github.com/acme/api.git",
                "https://github.com/acme/api.git",
            ),
            (
                "https://me:t0k3n@github.com/acme/api.git",
                "https://github.com/acme/api.git",
            ),
            (
                "HTTPS://me@dev.azure.com/org/p/_git/r",
                "https://dev.azure.com/org/p/_git/r",
            ),
            (
                "git@github.com:acme/api.git",
                "https://github.com/acme/api.git",
            ),
            (
                "ssh://git@ssh.github.com:443/acme/api.git",
                "https://github.com/acme/api.git",
            ),
            (
                "git@gitlab.com:group/sub/proj.git",
                "https://gitlab.com/group/sub/proj.git",
            ),
            (
                "git@bitbucket.org:ws/repo.git",
                "https://bitbucket.org/ws/repo.git",
            ),
            (
                "git@ssh.dev.azure.com:v3/org/proj/repo",
                "https://dev.azure.com/org/proj/_git/repo",
            ),
            (
                "ssh://org@vs-ssh.visualstudio.com/v3/org/proj/repo",
                "https://dev.azure.com/org/proj/_git/repo",
            ),
        ] {
            assert_eq!(https_form(remote).as_deref(), Ok(https), "{remote}");
        }
        for (remote, why) in [
            ("git@git.example.com:acme/api.git", "knows no HTTPS form"),
            (
                "git@ssh.dev.azure.com:v2/org/repo",
                "Azure DevOps SSH address",
            ),
            ("/home/u/other/api", "not an HTTPS or SSH address"),
            ("C:\\repos\\api", "not an HTTPS or SSH address"),
            ("http://example.org/api.git", "not an HTTPS or SSH address"),
            ("https://github.com/a b", "spaces or control characters"),
            ("https:///acme/api", "names no repository"),
            ("https://github.com", "names no repository"),
            ("git@github.com:acme/api?x", "SSH address Puddle can't read"),
        ] {
            let err = https_form(remote).unwrap_err();
            assert!(err.contains(why), "{remote}: {err}");
        }
    }

    #[test]
    fn the_host_is_where_git_and_curl_would_go_never_one_hidden_after_a_query() {
        // git and curl end the authority at `?`, `#` or `\`: the `@...` part is not a host.
        // Reading it any other way would aim origin at another host.
        for remote in [
            "https://github.com?@evil.example/acme/x.git",
            "https://evil.example#@github.com/x",
            "https://evil.example\\@github.com/x",
        ] {
            let err = https_form(remote).unwrap_err();
            assert!(err.contains("names no repository"), "{remote}: {err}");
        }
        let err = https_form("ssh://git@github.com?@evil.example/x").unwrap_err();
        assert!(err.contains("SSH address Puddle can't read"), "{err}");
    }

    #[test]
    fn a_refused_origin_never_shows_its_password() {
        for remote in [
            "http://me:s3cr3t@gitlab.internal/acme/x.git",
            "ssh://me:s3cr3t@git.example.com/acme/x.git",
            "https://me:s3cr3t@github.com",
        ] {
            let err = https_form(remote).unwrap_err();
            assert!(!err.contains("s3cr3t"), "{err}");
            assert!(err.contains("***@"), "{err}");
        }
    }

    fn git_dir(at: &Path) {
        std::fs::create_dir_all(at.join("objects")).unwrap();
        std::fs::create_dir_all(at.join("refs")).unwrap();
        std::fs::write(at.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    }

    #[test]
    fn a_checkout_is_found_from_its_folder_or_its_git_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let work = base.join("api");
        git_dir(&work.join(".git"));
        std::fs::write(
            work.join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/api.git\n",
        )
        .unwrap();
        for picked in [
            work.clone(),
            work.join(".git"),
            base.join("api").join(".").join(".git"),
        ] {
            let c = LocalCheckout::open(&picked).unwrap();
            assert_eq!(c.git_dir, work.join(".git"));
            assert_eq!(c.folder, work);
            assert_eq!(c.origin.as_deref(), Ok("https://github.com/acme/api.git"));
        }
        std::fs::remove_file(work.join(".git").join("config")).unwrap();
        let c = LocalCheckout::open(&work).unwrap();
        assert!(c.origin.unwrap_err().contains("no config file"));
        std::fs::write(work.join(".git").join("config"), "[core]\n").unwrap();
        assert!(
            LocalCheckout::open(&work)
                .unwrap()
                .origin
                .unwrap_err()
                .contains("no origin")
        );
    }

    #[test]
    fn what_is_not_a_checkout_is_refused_with_the_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let refused = |p: &Path, want: &str| {
            let err = LocalCheckout::open(p).unwrap_err();
            assert!(err.contains(want), "{}: {err}", p.display());
        };
        refused(Path::new("relative/api"), "not an absolute path");
        refused(&base.join("missing"), "can't be opened");
        std::fs::create_dir_all(base.join("plain")).unwrap();
        refused(&base.join("plain"), "has no .git");
        git_dir(&base.join("bare.git"));
        refused(&base.join("bare.git"), "bare repository");
        let linked = base.join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(linked.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        refused(&linked, "linked worktree or a submodule");
        let empty = base.join("empty");
        std::fs::create_dir_all(empty.join(".git")).unwrap();
        refused(&empty, "is not a git directory");
        let shared = base.join("shared").join(".git");
        git_dir(&shared);
        std::fs::create_dir_all(shared.join("objects").join("info")).unwrap();
        std::fs::write(
            shared.join("objects").join("info").join("alternates"),
            "/elsewhere/objects\n",
        )
        .unwrap();
        refused(&base.join("shared"), "alternates");
        let partial = base.join("partial").join(".git");
        git_dir(&partial);
        std::fs::write(
            partial.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/a/b\n\tpromisor = true\n\tpartialclonefilter = blob:none\n",
        )
        .unwrap();
        refused(&base.join("partial"), "partial clone");
    }

    #[test]
    fn a_config_that_is_too_large_or_not_a_file_gives_no_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let git = tmp.path().join(".git");
        git_dir(&git);
        std::fs::create_dir(git.join("config")).unwrap();
        assert!(
            read_config(&git.join("config"))
                .unwrap_err()
                .contains("no config file")
        );
        std::fs::remove_dir(git.join("config")).unwrap();
        let big = format!(
            "[remote \"origin\"]\n\turl = https://x/y\n{}",
            "#".repeat(usize::try_from(MAX_CONFIG_BYTES).unwrap())
        );
        std::fs::write(git.join("config"), big).unwrap();
        assert!(
            read_config(&git.join("config"))
                .unwrap_err()
                .contains("too large")
        );
        #[cfg(unix)]
        {
            std::fs::remove_file(git.join("config")).unwrap();
            std::os::unix::fs::symlink("/dev/zero", git.join("config")).unwrap();
            assert!(
                read_config(&git.join("config"))
                    .unwrap_err()
                    .contains("no config file")
            );
        }
    }

    #[test]
    fn the_import_commands_are_argument_vectors_and_every_git_process_gets_the_guard() {
        let layout = Layout::new(&WorkspaceId::new("acme").unwrap()).unwrap();
        let dir = CheckoutDir::new("api").unwrap();
        assert_eq!(import_mount(2).unwrap().as_str(), "/puddle/import/2/.git");
        let clone = layout.import_request(2, &dir).unwrap();
        assert_eq!(clone.program, "git");
        assert_eq!(
            clone.args,
            [
                "clone",
                "--quiet",
                "--no-local",
                "--no-checkout",
                "--origin",
                "puddle-checkout",
                "--",
                "/puddle/import/2",
                "/workspaces/acme/api",
            ]
        );
        let env = import_env(2).unwrap();
        assert_eq!(clone.env, env);
        let pairs: Vec<(String, String)> = (0..env
            .get("GIT_CONFIG_COUNT")
            .unwrap()
            .parse::<usize>()
            .unwrap())
            .map(|i| {
                (
                    env.get(&format!("GIT_CONFIG_KEY_{i}")).unwrap().to_owned(),
                    env.get(&format!("GIT_CONFIG_VALUE_{i}"))
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect();
        for (key, value) in [
            ("core.hooksPath", "/dev/null"),
            ("core.fsmonitor", "false"),
            ("safe.directory", "/puddle/import/2"),
            ("protocol.allow", "never"),
            ("protocol.file.allow", "always"),
            ("gc.auto", "0"),
            ("core.alternateRefsCommand", "true"),
        ] {
            assert!(
                pairs.contains(&(key.to_owned(), value.to_owned())),
                "{key}: {pairs:?}"
            );
        }
        assert_eq!(env.get("GIT_NO_LAZY_FETCH"), Some("1"));
        assert_eq!(env.get("GIT_LFS_SKIP_SMUDGE"), Some("1"));
        let url = "https://github.com/acme/a b'$(x)";
        let finish = layout.finish_import_request(2, &dir, url).unwrap();
        assert_eq!(finish.program, "sh");
        assert_eq!(finish.args[1], FINISH_IMPORT_SH);
        assert_eq!(
            finish.args[3..],
            ["/workspaces/acme/api", "/puddle/import/2", url]
        );
        assert_eq!(finish.env, env);
    }
}
