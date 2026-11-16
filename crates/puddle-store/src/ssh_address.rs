// SPDX-License-Identifier: AGPL-3.0-or-later
//! The HTTPS address of the repository an SSH address names, for the Git hosts Puddle ends SSH for.
//!
//! A workspace is cloned over HTTPS (Puddle supplies the credentials), so an SSH address typed
//! where a clone address is wanted (`git@github.com:acme/web.git`, `ssh://git@host:443/acme/web`)
//! is turned into the HTTPS address of the same repository. The hosts are the proxy's built-in SSH
//! hosts (`PathForm` in `puddle-proxy`'s `ssh_git/grammar.rs`); the UI reads addresses the same way
//! (`ui/src/lib/workspaces/model.ts`), and both are tested against one list of cases.

/// Why an SSH address can't be turned into an HTTPS one. The messages are for the user and say what
/// to use instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SshAddressError {
    /// Not an SSH address of the shapes Git has, or a path that is not a repository's.
    #[error(
        "that is not an SSH address Puddle can read; use git@github.com:owner/repo.git or the repository's HTTPS URL"
    )]
    Unreadable,
    /// An SSH address of a host Puddle does not end SSH for.
    #[error(
        "Puddle reads SSH addresses of GitHub, GitLab, Bitbucket and Azure DevOps; for another host, use the repository's HTTPS URL"
    )]
    OtherHost,
}

/// Whether `url` is written as an SSH address: `ssh://...` or the `host:path` form of `scp` (a colon
/// before any slash, and not a Windows drive letter).
#[must_use]
pub fn looks_like_ssh(url: &str) -> bool {
    let url = url.trim();
    url.get(..6)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("ssh://"))
        || scp_like(url).is_some()
}

/// `(user@host, path)` of a `host:path` address.
fn scp_like(url: &str) -> Option<(&str, &str)> {
    if url.contains("://") || url.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("http")) {
        return None;
    }
    let colon = url.find(':')?;
    // A slash before the colon is a path (`dir/a:b`), not a host.
    (colon > 1 && !url[..colon].contains('/')).then(|| (&url[..colon], &url[colon + 1..]))
}

/// The host and the path (without leading slashes) of an SSH address, or `None` when it has a
/// password, a port that is no number, no host or no path. The user name and the port are dropped:
/// the repository is the same whichever way it is reached.
fn split(url: &str) -> Option<(String, String)> {
    let is_url = url
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("ssh://"));
    let (authority, path) = if is_url {
        let rest = &url[6..];
        let slash = rest.find('/')?;
        (&rest[..slash], &rest[slash..])
    } else {
        scp_like(url)?
    };
    let (user, host_port) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(u, h)| (Some(u), h));
    if user.is_some_and(|u| u.is_empty() || u.contains([':', '@'])) {
        return None;
    }
    // Only the URL form has a port; `host:path` already used its colon.
    let host = match host_port.split_once(':') {
        None => host_port,
        Some((host, port))
            if is_url && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            host
        }
        Some(_) => return None,
    };
    let path = path.trim_start_matches('/');
    (!host.is_empty() && !host.contains(['@', '/']) && !path.is_empty())
        .then(|| (host.to_ascii_lowercase(), path.to_owned()))
}

/// The HTTPS address of the repository an SSH address names. The address is not checked beyond its
/// shape: the result goes through the same reading as any HTTPS address (`RepoRef::from_https_url`).
///
/// # Errors
///
/// [`SshAddressError::OtherHost`] for a host Puddle does not end SSH for, and
/// [`SshAddressError::Unreadable`] for an address that is not of the shape Git writes or whose
/// path holds a space, control character, `?`, `#` or `\`.
pub fn ssh_address_as_https(url: &str) -> Result<String, SshAddressError> {
    let url = url.trim();
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(SshAddressError::Unreadable);
    }
    let (host, path) = split(url).ok_or(SshAddressError::Unreadable)?;
    let known = matches!(
        host.as_str(),
        "github.com"
            | "ssh.github.com"
            | "gitlab.com"
            | "altssh.gitlab.com"
            | "bitbucket.org"
            | "altssh.bitbucket.org"
            | "ssh.dev.azure.com"
            | "vs-ssh.visualstudio.com"
    );
    if !known {
        return Err(SshAddressError::OtherHost);
    }
    if path.contains(['?', '#', '\\']) {
        return Err(SshAddressError::Unreadable);
    }
    let https_host = match host.as_str() {
        "github.com" | "ssh.github.com" => "github.com",
        "gitlab.com" | "altssh.gitlab.com" => "gitlab.com",
        "bitbucket.org" | "altssh.bitbucket.org" => "bitbucket.org",
        _ => return azure(&host, &path),
    };
    Ok(format!("https://{https_host}/{path}"))
}

/// `v3/<organisation>/<project>/<repository>` on Azure DevOps' SSH hosts.
fn azure(host: &str, path: &str) -> Result<String, SshAddressError> {
    let parts: Vec<&str> = path.split('/').collect();
    let [version, org, project, repo] = parts.as_slice() else {
        return Err(SshAddressError::Unreadable);
    };
    if *version != "v3" || [org, project, repo].iter().any(|p| p.is_empty()) {
        return Err(SshAddressError::Unreadable);
    }
    Ok(if host == "ssh.dev.azure.com" {
        format!("https://dev.azure.com/{org}/{project}/_git/{repo}")
    } else {
        format!("https://{org}.visualstudio.com/{project}/_git/{repo}")
    })
}

#[cfg(test)]
mod tests;
