// SPDX-License-Identifier: AGPL-3.0-or-later
//! The words of the problem a repository that could not be cloned raises.

/// The host of a repository address (`https://[user@]host[:port]/path`), `None` for anything else.
pub(super) fn url_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then_some(host)
}

/// The detail of a clone problem. `untrusted` is whether Puddle passed the host through because
/// its certificate was not accepted: then Windows' distrust comes first, git's words after it.
/// `made` is the sentence about a created workspace, empty otherwise.
pub(super) fn clone_detail(
    reason: &str,
    untrusted: bool,
    made: &str,
    url: &str,
    directory: &str,
) -> String {
    let lead = match url_host(url) {
        Some(host) if untrusted => format!(
            "Windows does not trust {host}'s certificate: add your company's root certificate \
             to Windows. Git said: {reason}"
        ),
        _ => reason.to_owned(),
    };
    format!(
        "{lead}. {made}The repository stays on the list: use Clone again to try once more, or \
         Remove from the list. To get it yourself, clone it in a terminal in the workspace: \
         git clone {url} /workspace/{directory}"
    )
}
