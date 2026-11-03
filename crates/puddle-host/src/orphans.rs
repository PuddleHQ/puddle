// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace disks the workspace list does not name: finding them and restoring one as a
//! workspace. Part of [`HostWorkspaces`](super::HostWorkspaces); the rules are in the storage spec.
//!
//! Puddle keeps such a disk and never removes it, so nothing here deletes anything. A restore
//! adds a list entry and writes nothing to the disk. What it reads from the disk (a checkout's
//! remote address) is guest input: it is parsed as an address and used only as one.

use puddle_api::{NewRepository, RepoUrl};
use puddle_types::VolumeName;
use puddle_workspace::{CheckoutList, FoundCheckout};

use super::*;

/// The key of the problem that names the unclaimed disks.
pub(crate) const PROBLEM_KEY: &str = "orphan-volumes";

/// The problem that names `ids`; `None` when there are none.
pub(crate) fn problem(ids: &[WorkspaceId]) -> Option<Problem> {
    if ids.is_empty() {
        return None;
    }
    let names: Vec<String> = ids.iter().map(|id| id.volume_name().to_string()).collect();
    let many = names.len() != 1;
    Some(Problem::new(
        PROBLEM_KEY,
        format!(
            "Puddle found {} workspace disk{} that its workspace list does not name",
            names.len(),
            if many { "s" } else { "" }
        ),
        format!(
            "{}. Puddle kept {} and changed nothing; it cannot tell from outside whether {} \
             hold any files. To use {} again, restore {} as a workspace from the workspace list. \
             Puddle never deletes {} by itself.",
            names.join(", "),
            if many { "them" } else { "it" },
            if many { "they" } else { "it" },
            if many { "them" } else { "it" },
            if many { "each one" } else { "it" },
            if many { "them" } else { "it" },
        ),
    ))
}

/// The list entries a disk's checkouts give: one per distinct readable remote address, in
/// directory order. When there is none, `asked` is the address to use, in the directory of the
/// disk's only checkout that has no readable address (it is that checkout, whose remote the user
/// is naming), else in the directory a clone of it would make.
///
/// # Errors
///
/// [`WorkspaceError::Invalid`] when no address could be read and none was given.
fn entries_for(
    list: &CheckoutList,
    asked: Option<RepoUrl>,
    volume: &VolumeName,
) -> Result<Vec<Repository>, WorkspaceError> {
    let mut found: Vec<Repository> = Vec::new();
    for checkout in &list.checkouts {
        let Some(entry) = readable(checkout) else {
            continue;
        };
        if !found.iter().any(|f| f.url == entry.url) {
            found.push(entry);
        }
    }
    if !found.is_empty() {
        return Ok(found);
    }
    let Some(url) = asked else {
        return Err(WorkspaceError::Invalid(format!(
            "Puddle could not read a repository address from {volume}; enter the address of the repository on it"
        )));
    };
    let mut unnamed = list
        .checkouts
        .iter()
        .filter(|c| c.unreadable.is_none())
        .filter_map(|c| CheckoutDir::new(&c.dir).ok());
    let directory = match (unnamed.next(), unnamed.next(), list.more) {
        (Some(only), None, 0) => only,
        _ => CheckoutDir::from_url(url.as_str()),
    };
    Ok(vec![Repository { url, directory }])
}

/// The entry for a checkout whose remote address Puddle could read and use.
fn readable(checkout: &FoundCheckout) -> Option<Repository> {
    if checkout.unreadable.is_some() {
        return None;
    }
    Some(Repository {
        url: RepoUrl::parse(checkout.remote_url.as_deref()?).ok()?,
        directory: CheckoutDir::new(&checkout.dir).ok()?,
    })
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// The workspace disks no listed workspace claims, in id order.
    pub(super) async fn orphan_volume_ids(&self) -> Result<Vec<WorkspaceId>, WorkspaceError> {
        let volumes = self.inner.runtime.list_volumes().await.map_err(|e| {
            WorkspaceError::Unavailable(format!("Puddle cannot list its workspace disks: {e}"))
        })?;
        let state = self.state();
        let mut ids: Vec<WorkspaceId> = volumes
            .iter()
            .filter_map(|v| v.volume_name()?.workspace_id())
            .filter(|id| !state.slots.contains_key(id))
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    /// Raises or ends the problem that names the unclaimed disks, to match what is there now.
    pub(super) async fn refresh_orphan_problem(&self) {
        match self.orphan_volume_ids().await {
            Ok(ids) => match problem(&ids) {
                Some(p) => self.inner.problems.raise(p),
                None => self.inner.problems.clear(PROBLEM_KEY),
            },
            Err(e) => {
                tracing::warn!(error = %e, "the unclaimed workspace disks could not be listed")
            }
        }
    }

    /// Whether the list now names `id` or a workspace called `name`.
    fn claimed(
        state: &State,
        id: &WorkspaceId,
        name: &WorkspaceName,
    ) -> Result<(), WorkspaceError> {
        if state.slots.contains_key(id)
            || puddle_api::label_taken(state.slots.values().map(|s| &s.record), name.as_str(), None)
                .is_some()
        {
            return Err(WorkspaceError::Conflict(format!(
                "a workspace named {name} is already in the list; nothing was replaced"
            )));
        }
        Ok(())
    }

    pub(super) async fn restore_orphan_volume(
        &self,
        id: &WorkspaceId,
        asked: Option<RepoUrl>,
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let name = WorkspaceName::new(id.as_str())
            .map_err(|e| WorkspaceError::Invalid(format!("this name can't be a workspace: {e}")))?;
        let volume = id.volume_name();
        let info = self
            .inner
            .runtime
            .volume(&volume)
            .await
            .map_err(|e| {
                WorkspaceError::Unavailable(format!("Puddle cannot look up {volume}: {e}"))
            })?
            .ok_or_else(|| WorkspaceError::NotFound(format!("no workspace disk {volume}")))?;
        {
            let state = self.state();
            if state.closed {
                return Err(WorkspaceError::Unavailable(
                    "Puddle is shutting down".to_owned(),
                ));
            }
            Self::claimed(&state, id, &name)?;
        }
        let listed = self
            .inner
            .workspaces
            .list_checkouts_of_unheld_volume(&self.inner.runtime, id)
            .await
            .map_err(|e| match e {
                puddle_workspace::WorkspaceError::InUse { .. } => {
                    WorkspaceError::Conflict(e.to_string())
                }
                puddle_workspace::WorkspaceError::NotFound { .. } => {
                    WorkspaceError::NotFound(format!("no workspace disk {volume}"))
                }
                other => WorkspaceError::Unavailable(format!(
                    "Puddle could not read {volume}, so it made no list entry: {other}"
                )),
            })?;
        let repositories = entries_for(&listed, asked, &volume)?;
        let memory = self.default_memory().map_err(|reason| {
            WorkspaceError::Conflict(format!(
                "{reason}, so Puddle can't tell how much memory the workspace gets; fix or reset the settings"
            ))
        })?;
        let mut record = WorkspaceRecord::new(id.clone(), name.clone(), repositories.clone());
        record.status = WorkspaceStatus::Stopped;
        record.memory = memory;
        record.created_at = self.inner.clock.now_ms();
        record.disk_size_mib = u64::from(info.size.as_mib());
        {
            let mut state = self.state();
            if state.closed {
                return Err(WorkspaceError::Unavailable(
                    "Puddle is shutting down".to_owned(),
                ));
            }
            // The disk was read without the lock, so claim it again here: a create or another
            // restore may have taken the name meanwhile, and an entry is never replaced.
            Self::claimed(&state, id, &name)?;
            state.slots.insert(
                id.clone(),
                Slot {
                    record: record.clone(),
                    creating: false,
                    deleting: false,
                    volume_missing: false,
                },
            );
            if let Err(e) = self.persist(&state) {
                state.slots.remove(id);
                return Err(WorkspaceError::Unavailable(format!(
                    "{name} was not restored: {e}; {SAVE_HINT}"
                )));
            }
        }
        tracing::info!(workspace = %name, volume = %volume, "a workspace disk was restored as a workspace");
        let new: Vec<NewRepository> = repositories
            .into_iter()
            .map(|repository| NewRepository {
                repository,
                identity: None,
                pull: true,
                push: true,
                local: None,
            })
            .collect();
        git.set_up(&name, &new).await;
        self.status_event(&record);
        self.refresh_orphan_problem().await;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(dir: &str, remote: Option<&str>) -> FoundCheckout {
        FoundCheckout {
            dir: dir.to_owned(),
            remote_url: remote.map(str::to_owned),
            remote_note: None,
            branch: None,
            head: None,
            dirty: false,
            unreadable: None,
        }
    }

    fn list(checkouts: Vec<FoundCheckout>) -> CheckoutList {
        CheckoutList {
            checkouts,
            ..CheckoutList::default()
        }
    }

    fn volume() -> VolumeName {
        VolumeName::new("ws-lost").unwrap()
    }

    #[test]
    fn the_problem_names_every_disk_without_saying_it_holds_data() {
        assert!(problem(&[]).is_none());
        let one = problem(&[WorkspaceId::new("a").unwrap()]).unwrap();
        assert_eq!(one.key, PROBLEM_KEY);
        assert!(one.title.contains("1 workspace disk that"), "{}", one.title);
        assert!(one.detail.contains("ws-a"), "{}", one.detail);
        assert!(!one.detail.contains("remove the volumes yourself"));
        let two = problem(&[
            WorkspaceId::new("a").unwrap(),
            WorkspaceId::new("b").unwrap(),
        ])
        .unwrap();
        assert!(two.title.contains("2 workspace disks"), "{}", two.title);
        assert!(two.detail.contains("ws-a, ws-b"), "{}", two.detail);
        assert!(
            two.detail.contains("cannot tell from outside"),
            "{}",
            two.detail
        );
    }

    #[test]
    fn readable_remotes_become_entries_once_each_in_directory_order() {
        let entries = entries_for(
            &list(vec![
                found("api", Some("https://example.org/a/api.git")),
                found("copy", Some("https://example.org/a/api.git")),
                found("web", Some("https://example.org/a/web")),
                found("ssh", Some("git@example.org:a/x.git")),
                found("bare", None),
            ]),
            Some(RepoUrl::parse("https://example.org/a/other").unwrap()),
            &volume(),
        )
        .unwrap();
        let shown: Vec<_> = entries
            .iter()
            .map(|e| (e.url.as_str().to_owned(), e.directory.to_string()))
            .collect();
        // The address the user gave is not needed, so it is not used.
        assert_eq!(
            shown,
            [
                ("https://example.org/a/api.git".to_owned(), "api".to_owned()),
                ("https://example.org/a/web".to_owned(), "web".to_owned()),
            ]
        );
    }

    #[test]
    fn a_disk_with_no_readable_address_needs_one_and_names_the_only_checkout() {
        let none = entries_for(&list(vec![found("bare", None)]), None, &volume());
        assert!(
            matches!(none, Err(WorkspaceError::Invalid(ref m)) if m.contains("enter the address") && m.contains("ws-lost")),
            "{none:?}"
        );
        let url = RepoUrl::parse("https://example.org/a/api").unwrap();
        let only = entries_for(
            &list(vec![found("bare", None)]),
            Some(url.clone()),
            &volume(),
        )
        .unwrap();
        assert_eq!(only[0].directory.to_string(), "bare");
        let two = entries_for(
            &list(vec![found("one", None), found("two", None)]),
            Some(url.clone()),
            &volume(),
        )
        .unwrap();
        assert_eq!(two[0].directory.to_string(), "api");
        let empty = entries_for(&list(vec![]), Some(url), &volume()).unwrap();
        assert_eq!(empty[0].directory.to_string(), "api");
    }
}
