// SPDX-License-Identifier: GPL-3.0-or-later
//! The checkouts a workspace holds and how its repository list relates to them: what the guest's
//! listing found, which list entry each checkout has, which checkouts "Manage this" is offered
//! for, and the checks that decide whether a repository may be added.
//!
//! Every string in [`Found`] came from the guest: clients escape it.

use puddle_store::{IdentityId, RepoRef};
use puddle_types::CheckoutDir;

use crate::workspaces::{MAX_REPOSITORIES, NewRepository, RepoUrl, Repository, WorkspaceError};

/// Why a checkout's remote address is not offered as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteNote {
    /// The address had a user name or password; it was removed.
    CredentialsRemoved,
    /// An SSH address.
    Ssh,
    /// Not an HTTPS address.
    NotHttps,
    /// No remote, or several and none called `origin`.
    NoRemote,
}

/// One checkout the guest's listing found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Its directory under the volume root.
    pub directory: String,
    /// The address of `origin` (else of the only remote) when it is an HTTPS address, with any
    /// user name or password removed.
    pub remote_url: Option<String>,
    /// Why there is no `remote_url`, or that credentials were removed from it.
    pub remote_note: Option<RemoteNote>,
    /// The branch; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// The short commit; `None` before the first commit.
    pub head: Option<String>,
    /// Uncommitted changes or untracked files.
    pub dirty: bool,
    /// Why Puddle could not read it, when it could not.
    pub unreadable: Option<String>,
}

/// What a listing found: the first checkouts and how many more there are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoundList {
    /// The checkouts, in directory order.
    pub checkouts: Vec<Found>,
    /// How many more there are.
    pub more: u64,
}

/// Whether "Manage this" is offered for a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manage {
    /// Whether it is offered.
    pub offered: bool,
    /// Why not, when it is not.
    pub reason: Option<String>,
}

/// One checkout, with the list entry that has its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    /// What the guest said.
    pub found: Found,
    /// The list entry with this directory; its address may differ from the checkout's remote.
    pub entry: Option<Repository>,
    /// Whether "Manage this" is offered.
    pub manage: Manage,
}

/// The checkouts of a workspace, as the API shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checkouts {
    /// Whether the workspace was running, so the listing is from this moment.
    pub running: bool,
    /// When the listing was made (epoch ms); `None` before the first one.
    pub checked_at: Option<u64>,
    /// Why there is no listing, or `None`.
    pub problem: Option<String>,
    /// How many checkouts there are past the listed ones.
    pub more: u64,
    /// The checkouts found.
    pub checkouts: Vec<Checkout>,
    /// The directories of list entries that are not on the volume. Left empty when the listing
    /// stopped at its limit: an entry past it may be there.
    pub missing: Vec<CheckoutDir>,
}

/// Relates a listing to the workspace's list `entries`: which entry each checkout has, whether it
/// may be managed, and which entries are missing. `listing` is `None` when there is none yet
/// (`problem` then says why, if it is known).
#[must_use]
pub fn checkouts_view(
    running: bool,
    checked_at: Option<u64>,
    problem: Option<String>,
    listing: Option<&FoundList>,
    entries: &[Repository],
) -> Checkouts {
    let Some(listing) = listing else {
        return Checkouts {
            running,
            checked_at,
            problem,
            ..Checkouts::default()
        };
    };
    let checkouts = listing
        .checkouts
        .iter()
        .map(|found| {
            let entry = entries
                .iter()
                .find(|e| e.directory.as_str() == found.directory)
                .cloned();
            let manage = manage_of(found, entry.as_ref(), entries);
            Checkout {
                found: found.clone(),
                entry,
                manage,
            }
        })
        .collect();
    let missing = if listing.more == 0 {
        entries
            .iter()
            .filter(|e| {
                !listing
                    .checkouts
                    .iter()
                    .any(|f| f.directory == e.directory.as_str() && f.unreadable.is_none())
            })
            .map(|e| e.directory.clone())
            .collect()
    } else {
        Vec::new()
    };
    Checkouts {
        running,
        checked_at,
        problem,
        more: listing.more,
        checkouts,
        missing,
    }
}

fn not_offered(reason: impl std::fmt::Display) -> Manage {
    Manage {
        offered: false,
        reason: Some(reason.to_string()),
    }
}

/// Why a checkout whose remote address is not an `https://` one cannot be managed.
#[must_use]
pub fn why_no_remote(note: Option<RemoteNote>) -> &'static str {
    match note {
        Some(RemoteNote::Ssh) => "its remote is an SSH address; Manage this needs an https:// one",
        Some(RemoteNote::NotHttps) => "its remote is not an https:// address",
        _ => "it has no remote Puddle can use",
    }
}

fn manage_of(found: &Found, entry: Option<&Repository>, entries: &[Repository]) -> Manage {
    if entry.is_some() {
        return not_offered("this checkout is already on the list");
    }
    if let Some(why) = &found.unreadable {
        return not_offered(format!("Puddle could not read this checkout: {why}"));
    }
    let Some(url) = &found.remote_url else {
        return not_offered(why_no_remote(found.remote_note));
    };
    let url = match RepoUrl::parse(url) {
        Ok(url) => url,
        Err(why) => return not_offered(why),
    };
    if entries.len() >= MAX_REPOSITORIES {
        return not_offered(format!(
            "the list is full ({MAX_REPOSITORIES} repositories)"
        ));
    }
    if let Some(listed) = entries.iter().find(|e| same_repository(&e.url, &url)) {
        return not_offered(format!(
            "this repository is already on the list, in {}",
            listed.directory
        ));
    }
    Manage {
        offered: true,
        reason: None,
    }
}

/// Whether two addresses name one repository, in the repository table's spelling.
fn same_repository(a: &RepoUrl, b: &RepoUrl) -> bool {
    match (
        RepoRef::from_https_url(a.as_str()),
        RepoRef::from_https_url(b.as_str()),
    ) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// What "Add repository" adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddRepository {
    /// Clone a new repository into a running workspace.
    Clone(NewRepository),
    /// Manage a checkout that is already on the volume ("Manage this"): its address is read from
    /// the guest at that moment.
    Existing {
        /// The checkout's directory.
        directory: CheckoutDir,
        /// The address the user was shown for it; the fresh read must still name the same
        /// repository ([`check_seen`]).
        seen: RepoUrl,
        /// The identity the user picked; `None` lets Puddle choose.
        identity: Option<IdentityId>,
        /// Pull on its new repository table row.
        pull: bool,
        /// Push on its new repository table row.
        push: bool,
    },
}

/// Refuses a "Manage this" whose checkout no longer has the address the user was shown, so a
/// workspace cannot swap the remote between the listing and the click.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] naming both addresses.
pub fn check_seen(
    directory: &CheckoutDir,
    seen: &RepoUrl,
    now: &RepoUrl,
) -> Result<(), WorkspaceError> {
    if same_repository(seen, now) {
        return Ok(());
    }
    Err(WorkspaceError::Conflict(format!(
        "the remote of {directory} changed since it was listed: it was {}, it is now {}",
        seen.as_str(),
        now.as_str()
    )))
}

/// Whether `new` may join `entries`: the list has room, its directory is not
/// taken and its repository is not on the list yet.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] naming what is in the way.
pub fn check_addition(entries: &[Repository], new: &Repository) -> Result<(), WorkspaceError> {
    if entries.len() >= MAX_REPOSITORIES {
        return Err(WorkspaceError::Conflict(format!(
            "the list is full: a workspace holds at most {MAX_REPOSITORIES} repositories"
        )));
    }
    if entries.iter().any(|e| e.entry == new.entry) {
        return Err(WorkspaceError::Conflict(
            "the new entry's id is already on the list; try again".into(),
        ));
    }
    if let Some(taken) = entries.iter().find(|e| e.directory == new.directory) {
        return Err(WorkspaceError::Conflict(format!(
            "{} is already on the list, as {}; give this one another directory",
            taken.directory, taken.url
        )));
    }
    if let Some(same) = entries.iter().find(|e| same_repository(&e.url, &new.url)) {
        return Err(WorkspaceError::Conflict(format!(
            "{} is already on the list, in {}",
            new.url, same.directory
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
