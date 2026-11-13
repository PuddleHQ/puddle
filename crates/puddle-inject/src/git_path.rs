// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which repository, and what kind of access, a request on a Git host is about.
//!
//! The push and pull lists and the choice of credential all hang on this reading, so it is
//! strict: a path is read only when it is in one canonical spelling, and a path that looks like a
//! Git request but is not (a dot segment, an encoded slash, `//`, a backslash, a NUL, a
//! percent-encoding that is not the canonical one) is refused, because the Git host may read it
//! as another repository than the one Puddle checked. A path that cannot be a Git request under
//! any reading is none of this module's business.
//!
//! Shapes read (case-insensitively, `.git` optional, as the hosts treat them):
//!
//! - GitHub and every other host: `/{owner}/{repo}[.git]/...`;
//! - Azure DevOps: `dev.azure.com/{org}/{project}/_git/{repo}/...`, `.../{org}/_git/{repo}/...`
//!   (the project has the repository's name) and `{org}.visualstudio.com/[DefaultCollection/]
//!   {project}/_git/{repo}/...`.
//!
//! followed by one of `info/refs`, `git-upload-pack`, `git-receive-pack` or `info/lfs/...`.

use puddle_store::RepoRef;

/// What a request asks of a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Reading: a fetch, a clone, an LFS download.
    Pull,
    /// Writing: a push, an LFS upload or lock.
    Push,
    /// A Git LFS batch request, which reads or writes depending on the `operation` in its body.
    Batch,
}

/// A request that names a repository and an access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitPath {
    /// The host asked for, lower-case.
    pub(crate) host: String,
    /// The host the credential is chosen for: Azure DevOps has several names for one service.
    pub(crate) credential_host: String,
    /// The user or organisation, lower-case.
    pub(crate) owner: String,
    /// `repo`, or `project/repo` on Azure DevOps, lower-case, without `.git`.
    pub(crate) repo: String,
    /// What the request asks of the repository.
    pub(crate) access: Access,
}

impl GitPath {
    /// The repository as the workspace's table spells it; `None` for a name the table cannot hold
    /// (a space in an Azure DevOps project, say).
    pub(crate) fn repo_ref(&self) -> Option<RepoRef> {
        RepoRef::new(&self.host, &self.owner, &self.repo).ok()
    }

    /// `host/owner/repo`, for messages.
    pub(crate) fn describe(&self) -> String {
        format!("{}/{}/{}", self.host, self.owner, self.repo)
    }
}

/// What a request on a Git host is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Classified {
    /// Not a Git request under any reading: nothing to decide here.
    NotGit,
    /// It may be a Git request, but not in one canonical spelling; the reason is one clause.
    Ambiguous(&'static str),
    /// A Git request for a repository.
    Git(GitPath),
}

/// The services' own words in a path, found after every decoding a server might apply.
const MARKERS: [&str; 4] = [
    "info/refs",
    "git-upload-pack",
    "git-receive-pack",
    "info/lfs",
];

/// Reads `method path?query` on `host` (lower-case, as the proxy normalises it).
pub(crate) fn classify(host: &str, method: &str, path: &str, query: Option<&str>) -> Classified {
    let segments = match canonical_segments(path) {
        Ok(segments) => segments,
        Err(why) if looks_like_git(path) => return Classified::Ambiguous(why),
        Err(_) => return Classified::NotGit,
    };
    let service = service(query);
    let has_service = !matches!(service, Ok(None));
    let found = match locate(host, &segments, has_service) {
        Ok(Some(found)) => found,
        Ok(None) => return Classified::NotGit,
        Err(why) => return Classified::Ambiguous(why),
    };
    let Some(endpoint) = Endpoint::of(&found.rest) else {
        return Classified::NotGit;
    };
    let service = match service {
        Ok(service) => service,
        Err(why) => return Classified::Ambiguous(why),
    };
    let reading = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
    let posting = method.eq_ignore_ascii_case("POST");
    let mut access = endpoint.access(reading, posting, service);
    // The words of a push anywhere after the repository, or in the query, make it one.
    if service == Some(Service::Receive) || found.rest.iter().any(|s| s == "git-receive-pack") {
        access = Access::Push;
    }
    Classified::Git(GitPath {
        host: host.to_owned(),
        credential_host: found.credential_host,
        owner: found.owner,
        repo: found.repo,
        access,
    })
}

/// The segments of `path`, each percent-decoded, when the path is in its one canonical spelling.
///
/// Canonical: it starts with `/`, has no empty segment, no `.` or `..`, no backslash, `;`,
/// control, non-ASCII or other byte a path may not carry raw (a space, `"`, `<`, `{`, ...), and
/// every `%XX` is upper-case hexadecimal for a byte that has to be encoded (never a letter, digit
/// or `-._~`, a separator, a control byte) and the decoded bytes are UTF-8.
fn canonical_segments(path: &str) -> Result<Vec<String>, &'static str> {
    let rest = path
        .strip_prefix('/')
        .ok_or("the path does not start with /")?;
    if rest.is_empty() {
        return Ok(Vec::new());
    }
    rest.split('/').map(decode_segment).collect()
}

fn decode_segment(part: &str) -> Result<String, &'static str> {
    if part.is_empty() {
        return Err("an empty path segment (// or a trailing /)");
    }
    if part == "." || part == ".." {
        return Err("a dot segment");
    }
    let mut out = Vec::with_capacity(part.len());
    let mut bytes = part.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'%' => {
                let high = bytes.next().and_then(upper_hex);
                let low = bytes.next().and_then(upper_hex);
                let (Some(high), Some(low)) = (high, low) else {
                    return Err("a percent-encoding that is not canonical");
                };
                let decoded = high << 4 | low;
                if is_unreserved(decoded)
                    || matches!(decoded, b'/' | b'\\' | b'%' | b'?' | b'#' | b';')
                    || decoded < 0x20
                    || decoded == 0x7f
                {
                    return Err("a percent-encoding that is not canonical");
                }
                out.push(decoded);
            }
            b'\\' => return Err("a backslash"),
            b';' => return Err("a path parameter (;)"),
            0..=0x1f | 0x7f => return Err("a control character"),
            0x80.. => return Err("a non-ASCII character"),
            other if is_unreserved(other) || SUB_DELIMS_AND_MORE.contains(&other) => {
                out.push(other);
            }
            _ => return Err("a character that must be percent-encoded"),
        }
    }
    String::from_utf8(out).map_err(|_| "an encoding that is not UTF-8")
}

/// The other bytes RFC 3986 lets a path segment carry as they are (`pchar`).
const SUB_DELIMS_AND_MORE: &[u8] = b"!$&'()*+,:=@";

fn upper_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn any_hex(byte: u8) -> Option<u8> {
    upper_hex(byte.to_ascii_uppercase())
}

fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Percent-decodes `text` leniently (any hexadecimal case, bad escapes left as they are).
fn decode_lossy(text: &str) -> String {
    let mut out = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        let escaped = match (byte, after) {
            (b'%', [high, low, escape_end @ ..]) => any_hex(*high)
                .zip(any_hex(*low))
                .map(|(high, low)| (high << 4 | low, escape_end)),
            _ => None,
        };
        if let Some((decoded, escape_end)) = escaped {
            out.push(decoded);
            rest = escape_end;
        } else {
            out.push(byte);
            rest = after;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `path` could be a Git request once a server has read it however it likes: decoded
/// (repeatedly), folded to lower case, backslashes as slashes, `.` and `..` resolved, empty
/// segments and `;` parameters dropped. Wider than the canonical reading on purpose.
fn looks_like_git(path: &str) -> bool {
    let mut text = path.to_owned();
    for _ in 0..4 {
        let decoded = decode_lossy(&text);
        if decoded == text {
            break;
        }
        text = decoded;
    }
    text.make_ascii_lowercase();
    let text = text.replace('\\', "/");
    let mut resolved: Vec<&str> = Vec::new();
    let mut plain: Vec<&str> = Vec::new();
    for segment in text.split('/') {
        let segment = segment.split(';').next().unwrap_or_default();
        match segment {
            "" | "." => {}
            ".." => {
                resolved.pop();
            }
            other => {
                resolved.push(other);
                plain.push(other);
            }
        }
    }
    [resolved.join("/"), plain.join("/")]
        .iter()
        .any(|joined| MARKERS.iter().any(|marker| joined.contains(marker)))
}

/// Where a repository sits in a path.
struct Located {
    credential_host: String,
    owner: String,
    repo: String,
    rest: Vec<String>,
}

fn is_git_segment(segment: &str) -> bool {
    segment.eq_ignore_ascii_case("_git")
}

fn repo_name(segment: &str) -> String {
    let lower = segment.to_ascii_lowercase();
    lower.strip_suffix(".git").unwrap_or(&lower).to_owned()
}

fn tail(segments: &[String], from: usize) -> Vec<String> {
    segments
        .iter()
        .skip(from)
        .map(|segment| segment.to_ascii_lowercase())
        .collect()
}

/// Finds the owner and the repository in `segments` by what `host` is. `has_service` says the
/// query names a `service`, which an `info/refs` request of the smart protocol always does.
///
/// # Errors
/// A reason, when the path could name more than one repository or one deeper than any host has.
fn locate(
    host: &str,
    segments: &[String],
    has_service: bool,
) -> Result<Option<Located>, &'static str> {
    let segment = |i: usize| segments.get(i).map(String::as_str);
    let lower = |i: usize| segments.get(i).map(|s| s.to_ascii_lowercase());
    if host == "dev.azure.com" {
        let (project, repo, rest_from) = if segment(2).is_some_and(is_git_segment) {
            (lower(1), segment(3).map(repo_name), 4)
        } else if segment(1).is_some_and(is_git_segment) {
            let repo = segment(2).map(repo_name);
            (repo.clone(), repo, 3)
        } else {
            return Ok(None);
        };
        let (Some(owner), Some(project), Some(repo)) = (lower(0), project, repo) else {
            return Ok(None);
        };
        return Ok(Some(Located {
            credential_host: host.to_owned(),
            owner,
            repo: format!("{project}/{repo}"),
            rest: tail(segments, rest_from),
        }));
    }
    if let Some(org) = host.strip_suffix(".visualstudio.com") {
        let (project, repo, rest_from) = if segment(1).is_some_and(is_git_segment) {
            (lower(0), segment(2).map(repo_name), 3)
        } else if segment(0).is_some_and(|s| s.eq_ignore_ascii_case("DefaultCollection"))
            && segment(2).is_some_and(is_git_segment)
        {
            (lower(1), segment(3).map(repo_name), 4)
        } else {
            return Ok(None);
        };
        let (Some(project), Some(repo)) = (project, repo) else {
            return Ok(None);
        };
        return Ok(Some(Located {
            credential_host: "dev.azure.com".to_owned(),
            owner: org.to_ascii_lowercase(),
            repo: format!("{project}/{repo}"),
            rest: tail(segments, rest_from),
        }));
    }
    let Some(owner) = lower(0) else {
        return Ok(None);
    };
    let flat = |rest_from: usize| -> Option<Located> {
        Some(Located {
            credential_host: host.to_owned(),
            owner: owner.clone(),
            repo: repo_name(segment(1)?),
            rest: tail(segments, rest_from),
        })
    };
    if RepoRef::max_segments(host) == 1 {
        return Ok(flat(2));
    }
    nested(host, segments, has_service, &owner).unwrap_or_else(|| Ok(flat(2)))
}

/// The Git endpoint of a request whose repository sits in nested groups
/// (`/group/subgroup/project.git/info/refs`), if its path says so: the words of a Git service
/// after at least two names. Only the services' own words count: a bare `info/refs` with no
/// `service` is a file of that name on the host's web pages, and anything under a `/-/` is
/// a page too. `None` means no such reading: the caller reads the flat shape.
///
/// `Some(Err(_))` is a path that could be read as two repositories, or one deeper than a host
/// has: refused rather than guessed.
fn nested(
    host: &str,
    segments: &[String],
    has_service: bool,
    owner: &str,
) -> Option<Result<Option<Located>, &'static str>> {
    let ends: Vec<usize> = (3..segments.len())
        .filter(|&at| {
            let rest = tail(segments, at);
            match Endpoint::of(&rest) {
                Some(Endpoint::InfoRefs) => has_service,
                Some(_) => true,
                None => false,
            }
        })
        .collect();
    let flat_read = Endpoint::of(&tail(segments, 2)).is_some();
    let at = match (ends.as_slice(), flat_read) {
        ([], _) => return None,
        ([at], false) => *at,
        _ => return Some(Err("the path could name more than one repository")),
    };
    let names = segments.get(1..at)?;
    if names.iter().any(|name| name == "-") {
        return None;
    }
    if names.len() > RepoRef::max_segments(host) {
        return Some(Err("a repository path deeper than any Git host has"));
    }
    let last = repo_name(names.last()?);
    let mut repo: Vec<String> = names
        .iter()
        .take(names.len() - 1)
        .map(|name| name.to_ascii_lowercase())
        .collect();
    repo.push(last);
    Some(Ok(Some(Located {
        credential_host: host.to_owned(),
        owner: owner.to_owned(),
        repo: repo.join("/"),
        rest: tail(segments, at),
    })))
}

/// The service a request reaches.
enum Endpoint {
    InfoRefs,
    UploadPack,
    ReceivePack,
    /// `info/lfs/...`: what follows `info/lfs`.
    Lfs(Vec<String>),
}

impl Endpoint {
    fn of(rest: &[String]) -> Option<Self> {
        let words: Vec<&str> = rest.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["info", "refs"] => Some(Self::InfoRefs),
            ["git-upload-pack"] => Some(Self::UploadPack),
            ["git-receive-pack"] => Some(Self::ReceivePack),
            ["info", "lfs", more @ ..] => {
                Some(Self::Lfs(more.iter().map(ToString::to_string).collect()))
            }
            _ => None,
        }
    }

    /// What it asks for. Anything that is not clearly a read is a write: a method that changes
    /// state, a service this reading does not know.
    fn access(&self, reading: bool, posting: bool, service: Option<Service>) -> Access {
        let by_method = if reading { Access::Pull } else { Access::Push };
        match self {
            Self::InfoRefs => match service {
                Some(Service::Upload) if reading => Access::Pull,
                None => by_method,
                Some(_) => Access::Push,
            },
            Self::UploadPack => {
                if posting || reading {
                    Access::Pull
                } else {
                    Access::Push
                }
            }
            Self::ReceivePack => Access::Push,
            Self::Lfs(more) => {
                let words: Vec<&str> = more.iter().map(String::as_str).collect();
                match words.as_slice() {
                    ["objects", "batch"] if posting => Access::Batch,
                    ["objects", "verify"] | ["locks", _, "unlock"] => Access::Push,
                    ["locks", "verify"] if posting => Access::Pull,
                    _ => by_method,
                }
            }
        }
    }
}

/// The `service` of an `info/refs` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Service {
    Upload,
    Receive,
    Other,
}

/// The one `service` parameter in `query`, if there is one.
fn service(query: Option<&str>) -> Result<Option<Service>, &'static str> {
    let mut found = None;
    for pair in query.unwrap_or_default().split(['&', ';']) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if !decode_lossy(key).eq_ignore_ascii_case("service") {
            continue;
        }
        if found.is_some() {
            return Err("more than one service in the query");
        }
        let value = decode_lossy(value).to_ascii_lowercase();
        found = Some(match value.as_str() {
            "git-upload-pack" => Service::Upload,
            "git-receive-pack" => Service::Receive,
            _ => Service::Other,
        });
    }
    Ok(found)
}

#[cfg(test)]
mod tests;
