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

fn not_offered(reason: impl Into<String>) -> Manage {
    Manage {
        offered: false,
        reason: Some(reason.into()),
    }
}

/// Why a checkout whose remote address is not an `https://` one cannot be managed.
#[must_use]
pub fn why_no_remote(note: Option<RemoteNote>) -> &'static str {
    match note {
        Some(RemoteNote::Ssh) => "SSH is not supported yet",
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
        /// The identity the user picked; `None` lets Puddle choose.
        identity: Option<IdentityId>,
        /// Pull on its new repository table row.
        pull: bool,
        /// Push on its new repository table row.
        push: bool,
    },
}

/// Whether `new` may join `entries` (storage ST-51): the list has room, its directory is not
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
mod tests {
    use super::*;

    fn entry(url: &str, dir: &str) -> Repository {
        Repository {
            url: RepoUrl::parse(url).unwrap(),
            directory: CheckoutDir::new(dir).unwrap(),
        }
    }

    fn found(dir: &str, url: Option<&str>, note: Option<RemoteNote>) -> Found {
        Found {
            directory: dir.to_owned(),
            remote_url: url.map(str::to_owned),
            remote_note: note,
            branch: Some("main".into()),
            head: Some("abc".into()),
            dirty: false,
            unreadable: None,
        }
    }

    fn view(listing: &FoundList, entries: &[Repository]) -> Checkouts {
        checkouts_view(true, Some(5), None, Some(listing), entries)
    }

    #[test]
    fn a_checkout_that_could_not_be_read_or_has_a_bad_address_is_not_offered() {
        let mut unreadable = found("odd", None, None);
        unreadable.unreadable = Some("permission denied".to_owned());
        let listing = FoundList {
            checkouts: vec![
                unreadable,
                found("plain", Some("http://github.com/o/plain"), None),
            ],
            more: 0,
        };
        let v = view(&listing, &[]);
        let why = |d: &str| {
            let c = v.checkouts.iter().find(|c| c.found.directory == d).unwrap();
            assert!(!c.manage.offered);
            c.manage.reason.clone().unwrap()
        };
        assert_eq!(
            why("odd"),
            "Puddle could not read this checkout: permission denied"
        );
        assert!(why("plain").contains("https://"), "{}", why("plain"));
    }

    #[test]
    fn addresses_of_a_host_the_table_cannot_name_are_compared_as_written() {
        let a = RepoUrl::parse("https://example.com:8443/a/b").unwrap();
        let same = RepoUrl::parse("https://example.com:8443/a/b").unwrap();
        let other = RepoUrl::parse("https://example.com:8443/a/c").unwrap();
        assert!(same_repository(&a, &same));
        assert!(!same_repository(&a, &other));
    }

    #[test]
    fn each_checkout_has_its_entry_and_a_reason_when_it_cannot_be_managed() {
        let entries = [entry("https://github.com/o/api", "api")];
        let listing = FoundList {
            checkouts: vec![
                found("api", Some("https://github.com/o/api"), None),
                found("web", Some("https://github.com/o/web"), None),
                found("ssh", None, Some(RemoteNote::Ssh)),
                found("old", None, Some(RemoteNote::NotHttps)),
                found("bare", None, Some(RemoteNote::NoRemote)),
                found("again", Some("https://github.com/O/API"), None),
            ],
            more: 0,
        };
        let v = view(&listing, &entries);
        let by = |d: &str| v.checkouts.iter().find(|c| c.found.directory == d).unwrap();
        assert!(by("api").entry.is_some());
        assert!(!by("api").manage.offered);
        assert!(by("web").manage.offered && by("web").entry.is_none());
        assert_eq!(
            by("ssh").manage.reason.as_deref(),
            Some("SSH is not supported yet")
        );
        assert!(
            by("old")
                .manage
                .reason
                .as_deref()
                .unwrap()
                .contains("https://")
        );
        assert!(
            by("bare")
                .manage
                .reason
                .as_deref()
                .unwrap()
                .contains("no remote")
        );
        let again = by("again").manage.reason.clone().unwrap();
        assert!(again.contains("already on the list, in api"), "{again}");
        assert_eq!(v.missing, Vec::new());
    }

    #[test]
    fn an_entry_without_a_checkout_is_missing_unless_the_listing_was_cut_short() {
        let entries = [entry("https://h/o/a", "a"), entry("https://h/o/b", "b")];
        let mut listing = FoundList {
            checkouts: vec![found("a", Some("https://h/o/a"), None)],
            more: 0,
        };
        let names =
            |v: &Checkouts| -> Vec<String> { v.missing.iter().map(ToString::to_string).collect() };
        assert_eq!(names(&view(&listing, &entries)), ["b"]);
        // An unreadable checkout is not proof the entry's folder is a checkout.
        listing.checkouts[0].unreadable = Some("boom".into());
        assert_eq!(names(&view(&listing, &entries)), ["a", "b"]);
        // Past the limit an entry may be among the checkouts left out.
        listing.checkouts[0].unreadable = None;
        listing.more = 3;
        let v = view(&listing, &entries);
        assert_eq!(v.missing, Vec::new());
        assert_eq!(v.more, 3);
    }

    #[test]
    fn a_full_list_offers_nothing() {
        let entries: Vec<Repository> = (0..MAX_REPOSITORIES)
            .map(|i| entry(&format!("https://h/o/r{i}"), &format!("r{i}")))
            .collect();
        let listing = FoundList {
            checkouts: vec![found("extra", Some("https://h/o/extra"), None)],
            more: 0,
        };
        let v = view(&listing, &entries);
        assert!(
            v.checkouts[0]
                .manage
                .reason
                .as_deref()
                .unwrap()
                .contains("full")
        );
    }

    #[test]
    fn no_listing_says_why() {
        let v = checkouts_view(false, None, Some("start it".into()), None, &[]);
        assert!(!v.running && v.checked_at.is_none());
        assert_eq!(v.problem.as_deref(), Some("start it"));
        assert_eq!(v.checkouts, Vec::new());
    }

    #[test]
    fn an_addition_needs_room_a_free_directory_and_a_new_repository() {
        let entries = vec![entry("https://github.com/o/api", "api")];
        assert!(check_addition(&entries, &entry("https://github.com/o/web", "web")).is_ok());
        let err = check_addition(&entries, &entry("https://github.com/o/web", "api")).unwrap_err();
        assert!(err.to_string().contains("already on the list, as"), "{err}");
        let err =
            check_addition(&entries, &entry("https://github.com/O/API.git", "other")).unwrap_err();
        assert!(err.to_string().contains("in api"), "{err}");
        let full: Vec<Repository> = (0..MAX_REPOSITORIES)
            .map(|i| entry(&format!("https://h/o/r{i}"), &format!("r{i}")))
            .collect();
        let err = check_addition(&full, &entry("https://h/o/new", "new")).unwrap_err();
        assert!(err.to_string().contains("full"), "{err}");
    }
}
