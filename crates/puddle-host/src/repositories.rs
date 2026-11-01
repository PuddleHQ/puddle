// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's repository list at run time: the checkouts on its volume, adding a repository
//! (a clone, or a checkout that is already there), cloning an entry again and taking one off the
//! list. Part of [`HostWorkspaces`](super::HostWorkspaces); the rules are in the storage spec.
//!
//! The list is saved before a call answers, so a save that fails changes nothing. Everything the
//! guest says about a checkout is hostile input (`puddle-workspace` parses it) and is only shown,
//! or used when the user picks "Manage this", and then it is read again at that moment.

use puddle_api::{
    AddRepository, Checkouts, Found, FoundList, NewRepository, RemoteNote, RepoUrl, check_addition,
    check_seen, checkouts_view, why_no_remote,
};

use super::*;

/// What the last listing of one workspace found, when it started and when it ended (epoch ms). Its lock is held while a
/// listing runs, so calls at the same moment share one.
pub(super) type CachedListing = Arc<tokio::sync::Mutex<Option<(u64, u64, FoundList)>>>;

fn note(n: puddle_workspace::RemoteNote) -> RemoteNote {
    match n {
        puddle_workspace::RemoteNote::CredentialsRemoved => RemoteNote::CredentialsRemoved,
        puddle_workspace::RemoteNote::Ssh => RemoteNote::Ssh,
        puddle_workspace::RemoteNote::NotHttps => RemoteNote::NotHttps,
        puddle_workspace::RemoteNote::NoRemote => RemoteNote::NoRemote,
    }
}

fn found(list: &puddle_workspace::CheckoutList) -> FoundList {
    FoundList {
        more: list.more,
        checkouts: list
            .checkouts
            .iter()
            .map(|c| Found {
                directory: c.dir.clone(),
                remote_url: c.remote_url.clone(),
                remote_note: c.remote_note.map(note),
                branch: c.branch.clone(),
                head: c.head.clone(),
                dirty: c.dirty,
                unreadable: c.unreadable.clone(),
            })
            .collect(),
    }
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    fn listing_cache(&self, id: &WorkspaceId) -> CachedListing {
        Arc::clone(
            self.inner
                .listings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(id.clone())
                .or_default(),
        )
    }

    /// The checkouts of workspace `id`: listed now when it runs, else the last listing.
    pub(super) async fn checkouts_of(&self, id: &WorkspaceId) -> Result<Checkouts, WorkspaceError> {
        let asked = self.inner.clock.now_ms();
        let record = self.get_record(id)?;
        let entries = record.repositories;
        let cache = self.listing_cache(id);
        let mut cached = cache.lock().await;
        let gated = if record.status == WorkspaceStatus::Running {
            self.gated(&record.name).await
        } else {
            Err("the workspace is not running; start it to see its checkouts".to_owned())
        };
        let gated = match gated {
            Ok(gated) => gated,
            Err(problem) => {
                let (at, kept) = cached
                    .as_ref()
                    .map_or((None, None), |(_, at, l)| (Some(*at), Some(l)));
                return Ok(checkouts_view(false, at, Some(problem), kept, &entries));
            }
        };
        // A listing that began after this call did, while it waited for the lock, is as good as
        // one of its own; one that began earlier may have read the volume before a change.
        if let Some((started, at, kept)) = cached.as_ref()
            && *started >= asked
        {
            return Ok(checkouts_view(true, Some(*at), None, Some(kept), &entries));
        }
        let began = self.inner.clock.now_ms();
        match self
            .inner
            .workspaces
            .list_checkouts(gated.ungated(), id)
            .await
        {
            Ok(listed) => {
                let (started, at, fresh) = (began, self.inner.clock.now_ms(), found(&listed));
                let view = checkouts_view(true, Some(at), None, Some(&fresh), &entries);
                *cached = Some((started, at, fresh));
                Ok(view)
            }
            Err(e) => {
                tracing::warn!(workspace = %record.name, error = %e, "the checkouts could not be listed");
                Ok(checkouts_view(
                    true,
                    None,
                    Some(e.to_string()),
                    None,
                    &entries,
                ))
            }
        }
    }

    /// Lists the checkouts of the running workspace `id` in the background, so a stopped
    /// workspace has a listing to show. Errors are logged by [`Self::checkouts_of`].
    pub(super) async fn refresh_checkouts_later(&self, id: &WorkspaceId) {
        let (this, id) = (self.clone(), id.clone());
        self.inner.tasks.lock().await.spawn(async move {
            let _ = this.checkouts_of(&id).await;
        });
    }

    fn get_record(&self, id: &WorkspaceId) -> Result<WorkspaceRecord, WorkspaceError> {
        self.state()
            .slots
            .get(id)
            .map(|s| s.record.clone())
            .ok_or_else(|| not_found(id))
    }

    /// The conflict that says why `record` cannot take a change to its checkouts now: it is busy,
    /// or it does not run.
    fn ready_to(record: &WorkspaceRecord, to: &str) -> Result<(), WorkspaceError> {
        if let Some(busy) = record.busy {
            return Err(WorkspaceError::Conflict(format!(
                "{} is busy ({busy}); wait for it to finish",
                record.name
            )));
        }
        if record.status != WorkspaceStatus::Running {
            return Err(WorkspaceError::Conflict(format!(
                "{} is {}; start it to {to}",
                record.name, record.status
            )));
        }
        Ok(())
    }

    /// The idle, running workspace's sandbox, or the conflict that says why not.
    async fn running_sandbox(
        &self,
        record: &WorkspaceRecord,
        to: &str,
    ) -> Result<Arc<GatedSandbox<R::Sandbox>>, WorkspaceError> {
        Self::ready_to(record, to)?;
        self.gated(&record.name)
            .await
            .map_err(WorkspaceError::Conflict)
    }

    /// Saves the list with `slot`'s repositories as `entries`; when the save fails the list is
    /// put back and the call is refused with nothing changed.
    fn save_repositories(
        &self,
        state: &mut State,
        id: &WorkspaceId,
        before: Vec<Repository>,
    ) -> Result<(), WorkspaceError> {
        if let Err(e) = self.persist(state) {
            if let Some(slot) = state.slots.get_mut(id) {
                slot.record.repositories = before;
            }
            return Err(WorkspaceError::Unavailable(format!(
                "the repository list was not saved, so nothing changed: {e}; {SAVE_HINT}"
            )));
        }
        Ok(())
    }

    fn repositories_changed(&self, name: &WorkspaceName) {
        self.inner.events.emit(Event::WorkspaceRepositoriesChanged {
            workspace: name.clone(),
        });
    }

    pub(super) async fn add_repository_to(
        &self,
        id: &WorkspaceId,
        add: AddRepository,
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        match add {
            AddRepository::Clone(new) => self.begin_clone(id, new, true, git).await,
            AddRepository::Existing {
                directory,
                seen,
                identity,
                pull,
                push,
            } => {
                self.manage_checkout(id, (&directory, &seen), (identity, pull, push), git)
                    .await
            }
        }
    }

    pub(super) async fn clone_entry_again(
        &self,
        id: &WorkspaceId,
        directory: &CheckoutDir,
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let repository = self
            .get_record(id)?
            .repositories
            .into_iter()
            .find(|r| &r.directory == directory)
            .ok_or_else(|| WorkspaceError::NotFound(format!("{directory} is not on the list")))?;
        let new = NewRepository {
            repository,
            identity: None,
            pull: true,
            push: true,
            local: None,
        };
        self.begin_clone(id, new, false, git).await
    }

    /// Accepts a clone of `new` into the running workspace `id`: a new entry when `add`, else the
    /// entry the list has already. The directory must not exist (Puddle never clones into a
    /// folder that holds anything). The workspace is busy `cloning` until the clone ends.
    async fn begin_clone(
        &self,
        id: &WorkspaceId,
        new: NewRepository,
        add: bool,
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let repository = new.repository.clone();
        let record = self.get_record(id)?;
        let gated = self.running_sandbox(&record, "add a repository").await?;
        let taken = self
            .inner
            .workspaces
            .exists_on_volume(gated.ungated(), id, &repository.directory)
            .await
            .map_err(|e| workspace_error(&e))?;
        if taken {
            return Err(WorkspaceError::Conflict(format!(
                "{} already exists in the workspace, and Puddle never clones into a folder that holds anything; choose another directory",
                repository.directory
            )));
        }
        let record = {
            let mut state = self.state();
            if state.closed {
                return Err(WorkspaceError::Unavailable(
                    "Puddle is shutting down".to_owned(),
                ));
            }
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            Self::ready_to(&slot.record, "add a repository")?;
            let before = slot.record.repositories.clone();
            if add {
                check_addition(&before, &repository)?;
                slot.record.repositories.push(repository.clone());
                self.save_repositories(&mut state, id, before)?;
            }
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            slot.record.busy = Some(Operation::Cloning);
            slot.record.clone()
        };
        git.set_up(&record.name, std::slice::from_ref(&new)).await;
        if add {
            self.repositories_changed(&record.name);
        }
        let this = self.clone();
        let work = record.clone();
        self.run(&record, Operation::Cloning, async move {
            this.clone_work(&work, &repository).await
        })
        .await;
        Ok(record)
    }

    /// The clone of an accepted add: one clone, one `sync`, a fresh listing. A clone git refuses
    /// is a background problem and the operation still ends `done`.
    async fn clone_work(
        &self,
        record: &WorkspaceRecord,
        repository: &Repository,
    ) -> Result<(), String> {
        let gated = self.gated(&record.name).await?;
        self.progress_cloning(&record.name, repository);
        let key = clone_problem_key(&record.name, &repository.directory);
        let cloned = self
            .inner
            .workspaces
            .clone_checkout(
                gated.ungated(),
                &record.id,
                repository.url.as_str(),
                &repository.directory,
                None,
            )
            .await;
        match cloned {
            Ok(_) => {
                self.inner.problems.clear(&key);
                self.inner
                    .workspaces
                    .sync(gated.ungated(), &record.id)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Err(puddle_workspace::WorkspaceError::Clone { reason, .. }) => {
                let listed = self.state().slots.get(&record.id).is_some_and(|s| {
                    s.record
                        .repositories
                        .iter()
                        .any(|r| r.directory == repository.directory)
                });
                tracing::warn!(workspace = %record.name, url = %repository.url, %reason, "a repository was not cloned");
                if listed {
                    self.raise_clone_problem(record, repository, &reason, false);
                }
            }
            Err(other) => return Err(other.to_string()),
        }
        self.refresh_checkouts_later(&record.id).await;
        Ok(())
    }

    /// The background problem of a repository that could not be cloned. It names the two ways
    /// out the workspace page offers and the command that gets the repository from a terminal.
    pub(super) fn raise_clone_problem(
        &self,
        record: &WorkspaceRecord,
        repository: &Repository,
        reason: &str,
        in_create: bool,
    ) {
        let made = if in_create {
            "The workspace was made without it. "
        } else {
            ""
        };
        self.inner.problems.raise(
            Problem::new(
                clone_problem_key(&record.name, &repository.directory),
                format!(
                    "Puddle could not clone {} into {}",
                    repository.url,
                    record.label()
                ),
                format!(
                    "{reason}. {made}The repository stays on the list: use Clone again to try \
                     once more, or Remove from the list. To get it yourself, clone it in a \
                     terminal in the workspace: git clone {} /workspaces/{}/{}",
                    repository.url, record.id, repository.directory
                ),
            )
            .about(record.name.clone()),
        );
    }

    /// "Manage this": the checkout `directory` that is on the volume joins the list. Its remote
    /// is read again in the guest now rather than taken from an earlier listing, and nothing in
    /// the checkout changes.
    async fn manage_checkout(
        &self,
        id: &WorkspaceId,
        (directory, seen): (&CheckoutDir, &RepoUrl),
        (identity, pull, push): (Option<puddle_store::IdentityId>, bool, bool),
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let record = self.get_record(id)?;
        let gated = self.running_sandbox(&record, "manage a checkout").await?;
        let now = self
            .inner
            .workspaces
            .read_checkout(gated.ungated(), id, directory)
            .await
            .map_err(|e| workspace_error(&e))?
            .ok_or_else(|| {
                WorkspaceError::Conflict(format!("{directory} is not a checkout in the workspace"))
            })?;
        if let Some(why) = &now.unreadable {
            return Err(WorkspaceError::Conflict(format!(
                "Puddle could not read {directory}: {why}"
            )));
        }
        let url = match &now.remote_url {
            Some(url) => RepoUrl::parse(url).map_err(WorkspaceError::Conflict)?,
            None => {
                return Err(WorkspaceError::Conflict(format!(
                    "cannot manage {directory}: {}",
                    why_no_remote(now.remote_note.map(note))
                )));
            }
        };
        check_seen(directory, seen, &url)?;
        let new = NewRepository {
            repository: Repository {
                url,
                directory: directory.clone(),
            },
            identity,
            pull,
            push,
            local: None,
        };
        let record = {
            let mut state = self.state();
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            Self::ready_to(&slot.record, "manage a checkout")?;
            let before = slot.record.repositories.clone();
            check_addition(&before, &new.repository)?;
            slot.record.repositories.push(new.repository.clone());
            let record = slot.record.clone();
            self.save_repositories(&mut state, id, before)?;
            record
        };
        git.set_up(&record.name, std::slice::from_ref(&new)).await;
        self.repositories_changed(&record.name);
        Ok(record)
    }

    /// Takes the entry for `directory` off the list; the checkout, the table row and the
    /// identities stay.
    pub(super) fn remove_entry(
        &self,
        id: &WorkspaceId,
        directory: &CheckoutDir,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let record = {
            let mut state = self.state();
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            if let Some(busy @ (Operation::Creating | Operation::Deleting)) = slot.record.busy {
                return Err(WorkspaceError::Conflict(format!(
                    "{} is {busy}; remove it from the list when that is done",
                    slot.record.label()
                )));
            }
            let before = slot.record.repositories.clone();
            slot.record
                .repositories
                .retain(|r| &r.directory != directory);
            if slot.record.repositories.len() == before.len() {
                return Err(WorkspaceError::NotFound(format!(
                    "{directory} is not on the list"
                )));
            }
            let record = slot.record.clone();
            self.save_repositories(&mut state, id, before)?;
            record
        };
        self.inner
            .problems
            .clear(&clone_problem_key(&record.name, directory));
        self.repositories_changed(&record.name);
        Ok(record)
    }
}
