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
//! repository's address (an SSH remote of a known Git host in its HTTPS form).
//!
//! In the guest every git call of the import carries `core.hooksPath=/dev/null` and
//! `core.fsmonitor=false`, so no hook and no monitor runs, whoever wrote them; git never copies a
//! repository's hooks into a clone anyway. The clone uses `--no-local`: git's own transport reads
//! the source through `upload-pack`, which takes no command from the source's config, instead of
//! copying or hard-linking its files by path.

use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_compute::ExecRequest;
use puddle_types::{CheckoutDir, GuestPath};

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
    /// A message for the user: the folder is missing, holds no `.git`, is a bare repository, or
    /// its `.git` is a worktree's or submodule's link file or not a git directory.
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
        puddle_compute::check_checkout_dir(&git_dir)
            .map_err(|why| format!("the checkout {} {why}", folder.display()))?;
        let origin = read_origin(&git_dir.join("config")).and_then(|url| https_form(&url));
        Ok(Self {
            git_dir,
            folder,
            origin,
        })
    }
}

/// The first `url` of `[remote "origin"]` in the git config file `config`, read as text: no
/// include is followed and nothing runs.
fn read_origin(config: &Path) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(config)
        .map_err(|_| "it has no config file, so no origin remote".to_owned())?;
    if !meta.is_file() {
        return Err("its config is not a plain file".into());
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err("its config file is too large to read".into());
    }
    let text = std::fs::read(config).map_err(|e| format!("its config can't be read: {e}"))?;
    origin_url(&String::from_utf8_lossy(&text)).ok_or_else(|| "it has no origin remote".into())
}

/// The first `url` in the `[remote "origin"]` section of git config text. A value git would
/// continue on the next line, or one it would read differently from this reader, counts as none.
fn origin_url(text: &str) -> Option<String> {
    let mut in_origin = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
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
        return config_value(value.trim());
    }
    None
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
/// for a value that continues on the next line or holds another escape.
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

/// The HTTPS form of a remote address: an `https://` address without its user name or password,
/// or an SSH remote of GitHub, GitLab, Bitbucket or Azure DevOps written as HTTPS.
///
/// # Errors
///
/// Why the address can't be used, for a remote of any other kind.
pub fn https_form(url: &str) -> Result<String, String> {
    let lower = url.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("https://") {
        let rest = &url[url.len() - rest.len()..];
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        return Ok(format!("https://{host}{path}"));
    }
    let (host, path) = if let Some(rest) = lower.strip_prefix("ssh://") {
        let rest = &url[url.len() - rest.len()..];
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host.split(':').next().unwrap_or(host);
        (
            host.to_ascii_lowercase(),
            path.trim_start_matches('/').to_owned(),
        )
    } else if let Some((authority, path)) = scp_like(url) {
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (
            host.to_ascii_lowercase(),
            path.trim_start_matches('/').to_owned(),
        )
    } else {
        return Err(format!(
            "its origin {url} is not an HTTPS or SSH address; give the repository's HTTPS address"
        ));
    };
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
                    "its origin {url} is an Azure DevOps SSH address Puddle can't read; \
                     give the repository's HTTPS address"
                )),
            }
        }
        _ => Err(format!(
            "its origin {url} is an SSH address of a host Puddle knows no HTTPS form for; \
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

/// The `-c` settings of every git call of an import: no hook and no file-system monitor runs,
/// and the mounted repository (owned by the host's user) is one git may read.
fn guarded(source: &str) -> Vec<String> {
    [
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        &format!("safe.directory={source}"),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The source folder in the guest of the `n`-th checkout (the parent of [`import_mount`]).
fn source(n: usize) -> String {
    format!("/puddle/import/{n}")
}

/// After the clone, in the clone (`$1`), from the source (`$2`) with `origin`'s address `$3`:
/// every branch and tag of the checkout, and what it knew of `origin`'s branches, become the
/// clone's own; the source remote goes; `origin` is the address; the branch that is checked out
/// follows its namesake on `origin` when the checkout knew one. The guard settings come first in
/// the arguments after `$3` and go before every git call.
pub const FINISH_IMPORT_SH: &str = r#"set -eu
dest=$1; src=$2; url=$3; shift 3
cd "$dest"
git "$@" fetch --quiet --update-head-ok "$src" '+refs/heads/*:refs/heads/*' '+refs/tags/*:refs/tags/*' '+refs/remotes/origin/*:refs/remotes/origin/*'
git "$@" remote remove puddle-checkout
git "$@" remote add origin "$url"
if branch=$(git "$@" symbolic-ref --quiet --short HEAD) && git "$@" rev-parse --quiet --verify "refs/remotes/origin/$branch" >/dev/null; then
  git "$@" branch --quiet --set-upstream-to="origin/$branch" "$branch"
fi
"#;

impl Layout {
    /// The clone of the `n`-th import: `git clone --no-local --origin puddle-checkout -- <source>
    /// <checkout>`, guarded (see [`FINISH_IMPORT_SH`]); an argument vector, never a shell string.
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
        let mut args = guarded(&src);
        args.extend(
            [
                "clone",
                "--quiet",
                "--no-local",
                "--origin",
                "puddle-checkout",
                "--",
                &src,
                dest.as_str(),
            ]
            .map(str::to_owned),
        );
        Ok(ExecRequest::new("git", args).with_timeout(IMPORT_TIMEOUT))
    }

    /// The step after [`Layout::import_request`]: [`FINISH_IMPORT_SH`] with the clone, the
    /// source, `origin`'s address and the guard settings as its arguments.
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
        let src = source(n);
        let mut args = vec![
            "-c".to_owned(),
            FINISH_IMPORT_SH.to_owned(),
            "puddle-import".to_owned(),
            dest.as_str().to_owned(),
            src.clone(),
            url.to_owned(),
        ];
        args.extend(guarded(&src));
        Ok(ExecRequest::new("sh", args).with_timeout(IMPORT_TIMEOUT))
    }
}

#[cfg(test)]
mod tests {
    use puddle_types::WorkspaceId;

    use super::*;

    #[test]
    fn the_origin_is_the_first_url_of_its_section_in_any_spelling() {
        let text = "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.org/up.git\n\
                    [Remote \"origin\"]  # mine\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                    \tURL = \"https://github.com/acme/api.git\" ; comment\n\turl = https://second\n";
        assert_eq!(
            origin_url(text).as_deref(),
            Some("https://github.com/acme/api.git")
        );
        assert_eq!(
            origin_url("[remote.origin]\nurl=git@github.com:acme/api.git\n").as_deref(),
            Some("git@github.com:acme/api.git")
        );
        // The subsection name is exact; nothing else counts.
        assert_eq!(origin_url("[remote \"Origin\"]\nurl = https://x/y\n"), None);
        assert_eq!(
            origin_url("[remote \"origin\"] junk\nurl = https://x/y\n"),
            None
        );
        assert_eq!(
            origin_url("[remote \"origin\"]\nfetch = x\n[core]\nurl = https://x/y\n"),
            None
        );
        assert_eq!(origin_url("url = https://x/y\n"), None);
        assert_eq!(origin_url(""), None);
    }

    #[test]
    fn a_value_this_reader_could_misread_counts_as_none() {
        assert_eq!(config_value(r#""a\"b\\c""#).as_deref(), Some(r#"a"b\c"#));
        assert_eq!(config_value(r"https://x/y \"), None);
        assert_eq!(config_value(r"https://x/\ty"), None);
        assert_eq!(config_value(r#""unclosed"#), None);
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
        ] {
            let err = https_form(remote).unwrap_err();
            assert!(err.contains(why), "{remote}: {err}");
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
    }

    #[test]
    fn a_config_that_is_too_large_or_not_a_file_gives_no_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let git = tmp.path().join(".git");
        git_dir(&git);
        std::fs::create_dir(git.join("config")).unwrap();
        assert!(
            read_origin(&git.join("config"))
                .unwrap_err()
                .contains("not a plain file")
        );
        std::fs::remove_dir(git.join("config")).unwrap();
        let big = format!(
            "[remote \"origin\"]\n\turl = https://x/y\n{}",
            "#".repeat(usize::try_from(MAX_CONFIG_BYTES).unwrap())
        );
        std::fs::write(git.join("config"), big).unwrap();
        assert!(
            read_origin(&git.join("config"))
                .unwrap_err()
                .contains("too large")
        );
    }

    #[test]
    fn the_import_commands_are_argument_vectors_with_every_git_call_guarded() {
        let layout = Layout::new(&WorkspaceId::new("acme").unwrap()).unwrap();
        let dir = CheckoutDir::new("api").unwrap();
        assert_eq!(import_mount(2).unwrap().as_str(), "/puddle/import/2/.git");
        let clone = layout.import_request(2, &dir).unwrap();
        assert_eq!(clone.program, "git");
        assert_eq!(
            clone.args,
            [
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "safe.directory=/puddle/import/2",
                "clone",
                "--quiet",
                "--no-local",
                "--origin",
                "puddle-checkout",
                "--",
                "/puddle/import/2",
                "/workspaces/acme/api",
            ]
        );
        let url = "https://github.com/acme/a b'$(x)";
        let finish = layout.finish_import_request(2, &dir, url).unwrap();
        assert_eq!(finish.program, "sh");
        assert_eq!(finish.args[1], FINISH_IMPORT_SH);
        assert_eq!(
            finish.args[3..6],
            ["/workspaces/acme/api", "/puddle/import/2", url]
        );
        assert_eq!(finish.args[6..], clone.args[..6]);
        // Every git call of the script passes the guard settings.
        let guarded_calls = FINISH_IMPORT_SH.matches("git \"$@\" ").count();
        assert_eq!(guarded_calls, 6, "{FINISH_IMPORT_SH}");
        assert_eq!(
            FINISH_IMPORT_SH.matches("git ").count(),
            6,
            "{FINISH_IMPORT_SH}"
        );
    }
}
