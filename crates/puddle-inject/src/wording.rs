// SPDX-License-Identifier: AGPL-3.0-or-later
//! The words of a Git refusal the workspace's allowed repositories cause.
//!
//! One term, "allowed repositories", is used here, on the workspace's Git tab, in the notice and in
//! the docs. The proxy puts `puddle: ` in front. The text says what Puddle did, then the one action to take first, then what
//! to run again; the switch that turns the check off is named on the Git tab, not here.

use puddle_types::GitAccess;

/// The guest's message for a push or fetch to `what` (`host/owner/repo`) that the allowed
/// repositories do not cover. `name` is the repository's own name when the notice can offer to
/// allow it; `None` when the name cannot be held in the table.
pub(crate) fn not_allowed(access: GitAccess, what: &str, name: Option<&str>) -> String {
    let (blocked, tick, again, switch) = match access {
        GitAccess::Push => (
            "push",
            "Push",
            "git push",
            "Only push to allowed repositories",
        ),
        GitAccess::Pull => (
            "fetch",
            "Pull",
            "the fetch or pull",
            "Only pull from allowed repositories",
        ),
    };
    match name {
        Some(name) => format!(
            "Blocked this {blocked}: {what} is not in this workspace's allowed repositories for {access}. Allow it in the Puddle notice (\"Allow {access} for {name}\"), or open the workspace's Git tab > Repositories and tick {tick} for it. Then run {again} again.",
            access = access_word(access),
        ),
        None => format!(
            "Blocked this {blocked}: {what} has a name the workspace's allowed repositories cannot hold. Open the workspace's Git tab > Repositories and turn off \"{switch}\" (this lets the workspace {blocked} to any repository its credential can reach). Then run {again} again.",
        ),
    }
}

fn access_word(access: GitAccess) -> &'static str {
    match access {
        GitAccess::Push => "push",
        GitAccess::Pull => "pull",
    }
}

#[cfg(test)]
mod tests;
