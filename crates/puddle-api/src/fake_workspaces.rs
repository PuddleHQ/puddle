// SPDX-License-Identifier: GPL-3.0-or-later
//! An in-memory [`WorkspaceService`] for tests and the UI fixture, with scripted progress.
//!
//! It follows the trait's contract the way the real service does: a call checks the request,
//! marks the workspace busy, returns it, and a background task walks the operation's steps,
//! emitting [`Event::StatusChanged`] and [`Event::WorkspaceProgress`] as it goes. Tests wait for
//! the end with [`FakeWorkspaces::idle`], and can make the next operation fail.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_store::Clock;
use puddle_types::{
    CheckoutDir, EntryId, Event, EventSink, SessionUser, WorkspaceId, WorkspaceStatus,
    WorkspaceStep,
};
use tokio::sync::watch;

use crate::checkouts::{AddRepository, Checkouts, FoundList, check_addition, checkouts_view};
use crate::workspaces::{
    AttachMode, Attached, BrowserTarget, DEFAULT_DISK_MIB, DEFAULT_IMAGE, DEFAULT_ROOT_DISK_MIB,
    DeleteCheck, DesktopTarget, DisplayName, GitSetup, KeptLogins, LaunchError, Launcher, Listing,
    LocalSource, NameIds, NewRepository, NewWorkspace, Operation, RepoFindings, ShellInitCheck,
    StartFolder, StartFolderChange, WorkspaceError, WorkspaceRecord, WorkspaceService,
};

/// One step an operation reports: the step, its detail, and the checkout a cloning step is about.
type Step = (WorkspaceStep, Option<String>, Option<CheckoutDir>);

/// A [`Launcher`] that opens nothing and remembers what it was asked to open.
#[derive(Debug, Default)]
pub struct FakeLauncher {
    opened: Mutex<Vec<WorkspaceId>>,
    browsers: Mutex<Vec<WorkspaceId>>,
    fail_with: Mutex<Option<String>>,
}

impl FakeLauncher {
    /// A launcher that succeeds.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next launch fail with this message (once).
    pub fn fail_next(&self, message: impl Into<String>) {
        *lock(&self.fail_with) = Some(message.into());
    }

    /// The workspaces opened in desktop VS Code so far, in order.
    #[must_use]
    pub fn opened(&self) -> Vec<WorkspaceId> {
        lock(&self.opened).clone()
    }

    /// The workspaces whose browser editor window was opened so far, in order.
    #[must_use]
    pub fn browsers_opened(&self) -> Vec<WorkspaceId> {
        lock(&self.browsers).clone()
    }
}

impl Launcher for FakeLauncher {
    fn open_desktop<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        _target: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            if let Some(message) = lock(&self.fail_with).take() {
                return Err(LaunchError::new(message));
            }
            lock(&self.opened).push(workspace.id.clone());
            Ok(())
        })
    }

    fn open_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        _target: &'a BrowserTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            if let Some(message) = lock(&self.fail_with).take() {
                return Err(LaunchError::new(message));
            }
            lock(&self.browsers).push(workspace.id.clone());
            Ok(())
        })
    }
}

/// What a seeded workspace would lose if it were deleted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unsaved {
    /// Per checkout.
    pub repos: Vec<RepoFindings>,
    /// Data outside any checkout.
    pub other: Listing,
    /// What the check could not read.
    pub errors: Vec<String>,
}

struct Entry {
    record: WorkspaceRecord,
    unsaved: Unsaved,
}

struct State {
    entries: BTreeMap<WorkspaceId, Entry>,
    fail_next: BTreeMap<Operation, String>,
    /// What a shell-init check in a running workspace answers; passes when unset.
    shell_init_check: Option<ShellInitCheck>,
    /// The scripts the checks were asked about.
    shell_init_checked: Vec<String>,
    /// The logins forgotten, as (workspace id, service), in order.
    forgotten_logins: Vec<(String, String)>,
    /// Whether forgetting a login fails as a credential store that refuses would.
    login_store_refuses: bool,
    /// The logins each workspace keeps, by workspace id; none when unset.
    kept_logins: BTreeMap<String, KeptLogins>,
    /// What each running workspace's guest holds: the checkouts a listing would find.
    guest: BTreeMap<WorkspaceId, FoundList>,
    /// The last listing made of each workspace, with its time (epoch ms).
    last_listing: BTreeMap<WorkspaceId, (u64, FoundList)>,
    /// The workspace disks the list does not name, each with the remote address a read of its
    /// checkout finds (`None`: no readable address).
    orphans: BTreeMap<WorkspaceId, Option<crate::workspaces::RepoUrl>>,
}

struct Inner {
    state: Mutex<State>,
    events: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    launcher: Arc<dyn Launcher>,
    step_delay: Duration,
    front: SocketAddr,
    in_flight: watch::Sender<usize>,
    /// Operations wait here while it is `true`.
    held: watch::Sender<bool>,
    /// How many times the API reported a settings change.
    settings_changes: std::sync::atomic::AtomicUsize,
}

/// The in-memory service. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct FakeWorkspaces {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for FakeWorkspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeWorkspaces")
            .field("workspaces", &lock(&self.inner.state).entries.len())
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl FakeWorkspaces {
    /// An empty service that emits into `events` and stamps creations from `clock`.
    #[must_use]
    pub fn new(events: Arc<dyn EventSink>, clock: Arc<dyn Clock>) -> Self {
        Self::with_options(events, clock, Arc::new(FakeLauncher::new()), Duration::ZERO)
    }

    /// Like [`FakeWorkspaces::new`], with the launcher desktop attaches go through and a pause
    /// between operation steps (zero for none; a few hundred milliseconds lets a page show them).
    #[must_use]
    pub fn with_options(
        events: Arc<dyn EventSink>,
        clock: Arc<dyn Clock>,
        launcher: Arc<dyn Launcher>,
        step_delay: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    entries: BTreeMap::new(),
                    fail_next: BTreeMap::new(),
                    shell_init_check: None,
                    shell_init_checked: Vec::new(),
                    forgotten_logins: Vec::new(),
                    login_store_refuses: false,
                    kept_logins: BTreeMap::new(),
                    guest: BTreeMap::new(),
                    last_listing: BTreeMap::new(),
                    orphans: BTreeMap::new(),
                }),
                events,
                clock,
                launcher,
                step_delay,
                front: SocketAddr::from(([127, 0, 0, 1], 18080)),
                in_flight: watch::channel(0).0,
                held: watch::channel(false).0,
                settings_changes: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    /// Adds a workspace as it is (no events). Replaces one with the same id.
    pub fn seed(&self, record: WorkspaceRecord, unsaved: Unsaved) {
        lock(&self.inner.state)
            .entries
            .insert(record.id.clone(), Entry { record, unsaved });
    }

    /// Makes the next `operation` fail after its steps with this reason, once. The workspace
    /// goes back to where it was (a failed create leaves nothing behind) and a
    /// [`WorkspaceStep::Failed`] event carries the reason.
    pub fn fail_next(&self, operation: Operation, reason: impl Into<String>) {
        lock(&self.inner.state)
            .fail_next
            .insert(operation, reason.into());
    }

    /// Makes the shell-init check of a running workspace answer `answer` from now on (it passes
    /// until this is called). A stopped workspace is never checked.
    pub fn set_shell_init_check(&self, answer: ShellInitCheck) {
        lock(&self.inner.state).shell_init_check = Some(answer);
    }

    /// The scripts the shell-init check was run on, in order.
    #[must_use]
    pub fn shell_init_checked(&self) -> Vec<String> {
        lock(&self.inner.state).shell_init_checked.clone()
    }

    /// The logins forgotten so far, as (workspace id, service).
    #[must_use]
    pub fn forgotten_logins(&self) -> Vec<(String, String)> {
        lock(&self.inner.state).forgotten_logins.clone()
    }

    /// Sets the logins workspace `id` keeps; forgetting one removes it from the list.
    pub fn set_kept_logins(&self, id: &WorkspaceId, kept: KeptLogins) {
        lock(&self.inner.state)
            .kept_logins
            .insert(id.to_string(), kept);
    }

    /// Adds a workspace disk the list does not name. `remote` is the address a read of its
    /// checkout finds; `None` makes a restore ask for one.
    pub fn add_orphan_volume(&self, id: &WorkspaceId, remote: Option<crate::workspaces::RepoUrl>) {
        lock(&self.inner.state).orphans.insert(id.clone(), remote);
    }

    /// Makes forgetting a login fail the way a credential store that refuses does.
    pub fn set_login_store_refuses(&self, refuses: bool) {
        lock(&self.inner.state).login_store_refuses = refuses;
    }

    /// Sets what `id`'s guest holds: the checkouts a listing of the running workspace finds.
    pub fn set_checkouts(&self, id: &WorkspaceId, checkouts: FoundList) {
        lock(&self.inner.state).guest.insert(id.clone(), checkouts);
    }

    /// Replaces what deleting `id` would lose. `false` if there is no such workspace.
    #[must_use]
    pub fn set_unsaved(&self, id: &WorkspaceId, unsaved: Unsaved) -> bool {
        lock(&self.inner.state)
            .entries
            .get_mut(id)
            .map(|e| e.unsaved = unsaved)
            .is_some()
    }

    /// Holds every operation before its next step until [`FakeWorkspaces::release`], so a test
    /// can look at the busy state.
    pub fn hold(&self) {
        self.inner.held.send_replace(true);
    }

    /// Lets held operations go on.
    pub fn release(&self) {
        self.inner.held.send_replace(false);
    }

    /// How many settings changes the API has reported to this service.
    #[must_use]
    pub fn settings_changes(&self) -> usize {
        self.inner
            .settings_changes
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolves when no operation is running.
    pub async fn idle(&self) {
        let mut rx = self.inner.in_flight.subscribe();
        // The sender lives in `inner`, which this borrow keeps alive.
        let _ = rx.wait_for(|n| *n == 0).await;
    }

    fn emit_status(&self, record: &WorkspaceRecord) {
        self.inner.events.emit(Event::StatusChanged {
            workspace: record.id.clone(),
            status: record.status,
        });
    }

    fn progress(&self, record: &WorkspaceRecord, (step, detail, repository): Step) {
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: record.id.clone(),
            step,
            detail,
            repository,
        });
    }

    /// Marks `id` busy with `op` after `check`, runs `enter` on the record (the immediate
    /// state change) and returns the record.
    fn accept(
        &self,
        id: &WorkspaceId,
        op: Operation,
        check: impl FnOnce(&WorkspaceRecord) -> Result<(), WorkspaceError>,
        enter: impl FnOnce(&mut WorkspaceRecord),
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let mut state = lock(&self.inner.state);
        let entry = state.entries.get_mut(id).ok_or_else(|| not_found(id))?;
        if let Some(busy) = entry.record.busy {
            return Err(WorkspaceError::Conflict(format!(
                "{} is busy ({busy}); wait for it to finish",
                entry.record.name
            )));
        }
        check(&entry.record)?;
        entry.record.busy = Some(op);
        enter(&mut entry.record);
        Ok(entry.record.clone())
    }

    /// Runs the rest of an operation on a task: the steps, then the end state.
    fn run(&self, record: &WorkspaceRecord, op: Operation, steps: Vec<Step>) {
        let this = self.clone();
        let record = record.clone();
        self.inner.in_flight.send_modify(|n| *n += 1);
        tokio::spawn(async move {
            for step in steps {
                this.pause().await;
                this.progress(&record, step);
            }
            this.pause().await;
            this.finish(&record.id, op);
            this.inner.in_flight.send_modify(|n| *n -= 1);
        });
    }

    async fn pause(&self) {
        let mut held = self.inner.held.subscribe();
        // The sender lives in `inner`, which `self` keeps alive.
        let _ = held.wait_for(|held| !*held).await;
        if self.inner.step_delay.is_zero() {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(self.inner.step_delay).await;
        }
    }

    fn finish(&self, id: &WorkspaceId, op: Operation) {
        let mut state = lock(&self.inner.state);
        let failure = state.fail_next.remove(&op);
        let Some(entry) = state.entries.get_mut(id) else {
            return;
        };
        entry.record.busy = None;
        let mut record = entry.record.clone();
        let mut status_changed = false;
        if let Some(reason) = failure {
            match op {
                Operation::Creating => {
                    state.entries.remove(id);
                }
                Operation::Starting => {
                    status_changed = true;
                    record.status = WorkspaceStatus::Crashed;
                }
                Operation::Stopping => {
                    // A failed graceful stop leaves the VM up.
                    status_changed = true;
                    record.status = WorkspaceStatus::Running;
                }
                Operation::Reclaiming
                | Operation::ResettingRootDisk
                | Operation::GrowingRootDisk
                | Operation::Cloning
                | Operation::Deleting => {}
            }
            if let Some(entry) = state.entries.get_mut(id) {
                entry.record = record.clone();
            }
            drop(state);
            if status_changed {
                self.emit_status(&record);
            }
            self.progress(&record, (WorkspaceStep::Failed, Some(reason), None));
            return;
        }
        match op {
            Operation::Creating | Operation::Cloning | Operation::GrowingRootDisk => {}
            Operation::Starting => {
                record.status = WorkspaceStatus::Running;
                status_changed = true;
            }
            Operation::Stopping => {
                record.status = WorkspaceStatus::Stopped;
                status_changed = true;
            }
            Operation::Reclaiming => {
                record.disk_used_mib = record.disk_used_mib.map(|used| used / 2);
            }
            Operation::ResettingRootDisk => {
                record.root_disk_used_mib = record.root_disk_size_mib.map(|_| 0);
            }
            Operation::Deleting => {
                state.entries.remove(id);
            }
        }
        if op != Operation::Deleting
            && let Some(entry) = state.entries.get_mut(id)
        {
            entry.record = record.clone();
        }
        drop(state);
        if status_changed {
            self.emit_status(&record);
        }
        self.progress(&record, (WorkspaceStep::Done, None, None));
    }

    fn repositories_changed(&self, record: &WorkspaceRecord) {
        self.inner.events.emit(Event::WorkspaceRepositoriesChanged {
            workspace: record.id.clone(),
        });
    }

    /// Clones `new` into the running workspace `id`: a new entry when `add`, else the entry it
    /// already is.
    async fn clone_into(
        &self,
        id: &WorkspaceId,
        new: NewRepository,
        add: bool,
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let guest_has = |state: &State| {
            state.guest.get(id).is_some_and(|g| {
                g.checkouts
                    .iter()
                    .any(|c| c.directory == new.repository.directory.as_str())
            })
        };
        let has_dir = guest_has(&lock(&self.inner.state));
        let record = self.accept(
            id,
            Operation::Cloning,
            |r| {
                if r.status != WorkspaceStatus::Running {
                    return Err(WorkspaceError::Conflict(format!(
                        "{} is {}; start it to add a repository",
                        r.name, r.status
                    )));
                }
                if add {
                    check_addition(&r.repositories, &new.repository)?;
                }
                if has_dir {
                    return Err(WorkspaceError::Conflict(format!(
                        "{} already exists in the workspace; Puddle never clones into a folder that holds anything",
                        new.repository.directory
                    )));
                }
                Ok(())
            },
            |r| {
                if add {
                    r.repositories.push(new.repository.clone());
                }
            },
        )?;
        git.set_up(&record.id, std::slice::from_ref(&new)).await;
        if add {
            self.repositories_changed(&record);
        }
        let repository = &new.repository;
        self.run(
            &record,
            Operation::Cloning,
            vec![(
                WorkspaceStep::Cloning,
                Some(repository.url.as_str().to_owned()),
                Some(repository.directory.clone()),
            )],
        );
        // The clone puts the checkout on the guest's disk, for the next listing.
        let found = crate::checkouts::Found {
            directory: repository.directory.as_str().to_owned(),
            remote_url: Some(repository.url.as_str().to_owned()),
            remote_note: None,
            branch: Some("main".to_owned()),
            head: Some("0000000".to_owned()),
            dirty: false,
            unreadable: None,
        };
        lock(&self.inner.state)
            .guest
            .entry(id.clone())
            .or_default()
            .checkouts
            .push(found);
        Ok(record)
    }

    /// "Manage this": the checkout `directory` the guest holds joins the list.
    async fn manage_existing(
        &self,
        id: &WorkspaceId,
        (directory, seen): (&CheckoutDir, &crate::workspaces::RepoUrl),
        identity: Option<puddle_store::IdentityId>,
        pull: bool,
        push: bool,
        git: &dyn GitSetup,
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let (name, new) = {
            let state = lock(&self.inner.state);
            let entry = state.entries.get(id).ok_or_else(|| not_found(id))?;
            let record = &entry.record;
            if let Some(busy) = record.busy {
                return Err(WorkspaceError::Conflict(format!(
                    "{} is busy ({busy}); wait for it to finish",
                    record.name
                )));
            }
            if record.status != WorkspaceStatus::Running {
                return Err(WorkspaceError::Conflict(format!(
                    "{} is {}; start it to manage a checkout",
                    record.name, record.status
                )));
            }
            let found = state
                .guest
                .get(id)
                .and_then(|g| {
                    g.checkouts
                        .iter()
                        .find(|c| c.directory == directory.as_str())
                })
                .ok_or_else(|| {
                    WorkspaceError::Conflict(format!(
                        "{directory} is not a checkout in the workspace"
                    ))
                })?;
            let url = found
                .remote_url
                .as_deref()
                .ok_or_else(|| {
                    WorkspaceError::Conflict(format!(
                        "cannot manage {directory}: {}",
                        crate::checkouts::why_no_remote(found.remote_note)
                    ))
                })
                .and_then(|u| {
                    crate::workspaces::RepoUrl::parse(u).map_err(WorkspaceError::Conflict)
                })?;
            crate::checkouts::check_seen(directory, seen, &url)?;
            let repository = crate::workspaces::Repository {
                entry: crate::workspaces::new_entry_id(|e| {
                    record.repositories.iter().any(|r| r.entry == *e)
                })
                .map_err(WorkspaceError::Unavailable)?,
                url,
                directory: directory.clone(),
            };
            check_addition(&record.repositories, &repository)?;
            (
                record.id.clone(),
                NewRepository {
                    repository,
                    identity,
                    pull,
                    push,
                    local: None,
                },
            )
        };
        git.set_up(&name, std::slice::from_ref(&new)).await;
        let record = {
            let mut state = lock(&self.inner.state);
            let entry = state.entries.get_mut(id).ok_or_else(|| not_found(id))?;
            entry.record.repositories.push(new.repository);
            entry.record.clone()
        };
        self.repositories_changed(&record);
        Ok(record)
    }

    fn check_for(&self, id: &WorkspaceId) -> Result<DeleteCheck, WorkspaceError> {
        let state = lock(&self.inner.state);
        let entry = state.entries.get(id).ok_or_else(|| not_found(id))?;
        Ok(delete_check(entry))
    }
}

fn not_found(id: &WorkspaceId) -> WorkspaceError {
    WorkspaceError::NotFound(format!("no workspace {id}"))
}

fn delete_check(entry: &Entry) -> DeleteCheck {
    let removes_sandbox = entry
        .record
        .status
        .is_down()
        .then(|| entry.record.id.sandbox_name());
    if entry.record.status == WorkspaceStatus::VolumeMissing {
        return DeleteCheck::new(
            entry.record.id.clone(),
            Vec::new(),
            Listing::default(),
            Vec::new(),
            removes_sandbox,
        )
        .with_volume_missing();
    }
    DeleteCheck::new(
        entry.record.id.clone(),
        entry.unsaved.repos.clone(),
        entry.unsaved.other.clone(),
        entry.unsaved.errors.clone(),
        removes_sandbox,
    )
}

/// A checkout on this computer as the fake sees it: a folder with a `.git/HEAD`, its origin the
/// first `url = ` line of `.git/config`. Only the shape a screen needs; the real service reads
/// much more carefully (`puddle-workspace`'s `LocalCheckout`).
fn fake_local_checkout(path: &str) -> Result<LocalSource, String> {
    let folder = std::path::Path::new(path.trim_end_matches(['/', '\\']));
    let folder = if folder.file_name().is_some_and(|n| n == ".git") {
        folder.parent().unwrap_or(folder)
    } else {
        folder
    };
    let git_dir = folder.join(".git");
    if !git_dir.join("HEAD").is_file() {
        return Err(format!(
            "{} is not a git checkout: it has no .git",
            folder.display()
        ));
    }
    let origin = std::fs::read_to_string(git_dir.join("config"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix("url = ").map(str::to_owned))
        })
        .ok_or_else(|| "it has no origin remote".to_owned());
    Ok(LocalSource {
        folder_name: folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        git_dir,
        origin,
    })
}

impl WorkspaceService for FakeWorkspaces {
    fn local_checkout(&self, path: &str) -> Result<LocalSource, String> {
        fake_local_checkout(path)
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<WorkspaceRecord>, WorkspaceError>> {
        Box::pin(async move {
            Ok(lock(&self.inner.state)
                .entries
                .values()
                .map(|e| e.record.clone())
                .collect())
        })
    }

    fn get<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            lock(&self.inner.state)
                .entries
                .get(id)
                .map(|e| e.record.clone())
                .ok_or_else(|| not_found(id))
        })
    }

    fn create<'a>(
        &'a self,
        new: NewWorkspace,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = {
                // The id is picked under the lock, so two creates cannot take the same one.
                let mut state = lock(&self.inner.state);
                // Ids come from the name, so a test or the UI fixture can name a workspace's
                // paths before it exists.
                let id = new
                    .id(&NameIds, |id| state.entries.contains_key(id))
                    .map_err(|e| WorkspaceError::Unavailable(e.to_string()))?;
                let mut record = WorkspaceRecord::new(id.clone(), new.name.clone(), new.entries());
                record.start_folder.clone_from(&new.start_folder);
                record.image = new
                    .image
                    .as_ref()
                    .map_or_else(|| DEFAULT_IMAGE.to_owned(), |i| i.as_str().to_owned());
                if let Some(memory) = new.memory {
                    record.memory = memory;
                }
                record.created_at = self.inner.clock.now_ms();
                record.disk_size_mib = DEFAULT_DISK_MIB;
                record.disk_used_mib = Some(0);
                record.root_disk_size_mib = Some(DEFAULT_ROOT_DISK_MIB);
                record.root_disk_used_mib = Some(0);
                record.busy = Some(Operation::Creating);
                if crate::label_taken(
                    state.entries.values().map(|e| &e.record),
                    new.name.as_str(),
                    None,
                )
                .is_some()
                {
                    return Err(WorkspaceError::Conflict(format!(
                        "a workspace named {} already exists",
                        new.name
                    )));
                }
                git.claim(&id, new.owner)?;
                state.entries.insert(
                    id,
                    Entry {
                        record: record.clone(),
                        unsaved: Unsaved::default(),
                    },
                );
                record
            };
            git.set_up(&record.id, &new.repositories).await;
            self.emit_status(&record);
            self.run(
                &record,
                Operation::Creating,
                vec![
                    (WorkspaceStep::PreparingVolume, None, None),
                    (
                        WorkspaceStep::PullingImage,
                        Some(record.image.clone()),
                        None,
                    ),
                ]
                .into_iter()
                .chain(record.repositories.iter().map(|r| {
                    (
                        WorkspaceStep::Cloning,
                        Some(r.url.as_str().to_owned()),
                        Some(r.directory.clone()),
                    )
                }))
                .collect(),
            );
            Ok(record)
        })
    }

    fn start<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
                id,
                Operation::Starting,
                |r| {
                    if r.status.is_down() {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is already {}",
                            r.name, r.status
                        )))
                    }
                },
                |r| r.status = WorkspaceStatus::Starting,
            )?;
            self.emit_status(&record);
            self.run(
                &record,
                Operation::Starting,
                vec![
                    (WorkspaceStep::Starting, None, None),
                    (WorkspaceStep::Syncing, None, None),
                ],
            );
            Ok(record)
        })
    }

    fn stop<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
                id,
                Operation::Stopping,
                |r| {
                    if r.status == WorkspaceStatus::Running {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is not running ({})",
                            r.name, r.status
                        )))
                    }
                },
                |r| r.status = WorkspaceStatus::Draining,
            )?;
            self.emit_status(&record);
            self.run(
                &record,
                Operation::Stopping,
                vec![
                    (WorkspaceStep::Stopping, None, None),
                    (WorkspaceStep::Reclaiming, None, None),
                ],
            );
            Ok(record)
        })
    }

    fn reclaim<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
                id,
                Operation::Reclaiming,
                |r| {
                    if r.status == WorkspaceStatus::Starting
                        || r.status == WorkspaceStatus::Draining
                    {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is {}; try again in a moment",
                            r.name, r.status
                        )))
                    } else {
                        Ok(())
                    }
                },
                |_| {},
            )?;
            self.run(
                &record,
                Operation::Reclaiming,
                vec![(WorkspaceStep::Reclaiming, None, None)],
            );
            Ok(record)
        })
    }

    fn reset_root_disk<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
                id,
                Operation::ResettingRootDisk,
                |r| {
                    if r.status.is_down() {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is {}; stop it before resetting its root disk",
                            r.name, r.status
                        )))
                    }
                },
                |_| {},
            )?;
            self.run(
                &record,
                Operation::ResettingRootDisk,
                vec![(WorkspaceStep::ResettingRootDisk, None, None)],
            );
            Ok(record)
        })
    }

    fn grow_root_disk<'a>(
        &'a self,
        id: &'a WorkspaceId,
        size_mib: u32,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
                id,
                Operation::GrowingRootDisk,
                |r| match r.root_disk_size_mib {
                    Some(current) if u64::from(size_mib) <= current => {
                        Err(WorkspaceError::Invalid(format!(
                            "a root disk only grows: {size_mib} MiB is not above {current} MiB"
                        )))
                    }
                    _ => Ok(()),
                },
                |r| {
                    r.root_disk_size_mib = Some(u64::from(size_mib));
                    r.root_disk_grown_mib = Some(size_mib);
                },
            )?;
            self.run(
                &record,
                Operation::GrowingRootDisk,
                vec![(WorkspaceStep::GrowingRootDisk, None, None)],
            );
            Ok(record)
        })
    }

    fn delete_check<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<DeleteCheck, WorkspaceError>> {
        Box::pin(async move { self.check_for(id) })
    }

    fn delete<'a>(
        &'a self,
        id: &'a WorkspaceId,
        fingerprint: Option<&'a str>,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let check = self.check_for(id)?;
            let record = self.accept(
                id,
                Operation::Deleting,
                |r| {
                    if !r.status.is_down() {
                        return Err(WorkspaceError::Conflict(format!(
                            "{} is {}; stop it before deleting it",
                            r.name, r.status
                        )));
                    }
                    match fingerprint {
                        Some(seen) if seen == check.fingerprint => Ok(()),
                        Some(_) => Err(WorkspaceError::Conflict(format!(
                            "{} has changed since it was checked; check it again",
                            r.name
                        ))),
                        None if check.is_clean() => Ok(()),
                        None => Err(WorkspaceError::Conflict(format!(
                            "{} has work that is not saved on a remote; review it and confirm",
                            r.name
                        ))),
                    }
                },
                |_| {},
            )?;
            self.run(
                &record,
                Operation::Deleting,
                vec![
                    (WorkspaceStep::Checking, None, None),
                    (WorkspaceStep::Removing, None, None),
                ],
            );
            Ok(record)
        })
    }

    fn rename<'a>(
        &'a self,
        id: &'a WorkspaceId,
        name: DisplayName,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let mut state = lock(&self.inner.state);
            crate::check_rename(state.entries.values().map(|e| &e.record), id, &name)?;
            let entry = state.entries.get_mut(id).ok_or_else(|| not_found(id))?;
            entry.record.name = name;
            Ok(entry.record.clone())
        })
    }

    fn set_start_folder<'a>(
        &'a self,
        id: &'a WorkspaceId,
        start_folder: Option<StartFolder>,
    ) -> BoxFuture<'a, Result<StartFolderChange, WorkspaceError>> {
        Box::pin(async move {
            let mut state = lock(&self.inner.state);
            crate::check_start_folder_change(state.entries.values().map(|e| &e.record), id)?;
            let entry = state.entries.get_mut(id).ok_or_else(|| not_found(id))?;
            entry.record.start_folder = start_folder;
            // A folder the fake was never told about "exists" only in a running workspace.
            let exists = (entry.record.status == WorkspaceStatus::Running
                && entry.record.start_folder.is_some())
            .then_some(true);
            Ok(StartFolderChange {
                record: entry.record.clone(),
                exists,
            })
        })
    }

    fn orphan_volumes(&self) -> BoxFuture<'_, Result<Vec<WorkspaceId>, WorkspaceError>> {
        Box::pin(async move { Ok(lock(&self.inner.state).orphans.keys().cloned().collect()) })
    }

    fn restore_orphan<'a>(
        &'a self,
        id: &'a WorkspaceId,
        repository: Option<crate::workspaces::RepoUrl>,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let name = DisplayName::parse(id.as_str()).map_err(|e| {
                WorkspaceError::Invalid(format!("this name can't be a workspace: {e}"))
            })?;
            let (record, new) = {
                let mut state = lock(&self.inner.state);
                let Some(found) = state.orphans.get(id).cloned() else {
                    return Err(WorkspaceError::NotFound(format!(
                        "no workspace disk {}",
                        id.volume_name()
                    )));
                };
                if state.entries.contains_key(id)
                    || crate::label_taken(
                        state.entries.values().map(|e| &e.record),
                        name.as_str(),
                        None,
                    )
                    .is_some()
                {
                    return Err(WorkspaceError::Conflict(format!(
                        "a workspace named {name} already exists; nothing was replaced"
                    )));
                }
                let Some(url) = found.or(repository) else {
                    return Err(WorkspaceError::Invalid(format!(
                        "Puddle could not read a repository address from {}; enter the address of the repository on it",
                        id.volume_name()
                    )));
                };
                let entry = crate::workspaces::Repository {
                    entry: crate::workspaces::new_entry_id(|_| false)
                        .map_err(WorkspaceError::Unavailable)?,
                    directory: CheckoutDir::from_url(url.as_str()),
                    url,
                };
                let mut record =
                    WorkspaceRecord::new(id.clone(), name.clone(), vec![entry.clone()]);
                record.created_at = self.inner.clock.now_ms();
                record.status = WorkspaceStatus::Stopped;
                state.orphans.remove(id);
                state.entries.insert(
                    id.clone(),
                    Entry {
                        record: record.clone(),
                        unsaved: Unsaved::default(),
                    },
                );
                let new = NewRepository {
                    repository: entry,
                    identity: None,
                    pull: true,
                    push: true,
                    local: None,
                };
                (record, new)
            };
            git.set_up(id, std::slice::from_ref(&new)).await;
            self.emit_status(&record);
            Ok(record)
        })
    }

    fn set_session_user<'a>(
        &'a self,
        id: &'a WorkspaceId,
        user: SessionUser,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let mut state = lock(&self.inner.state);
            crate::check_session_user_change(state.entries.values().map(|e| &e.record), id)?;
            let entry = state.entries.get_mut(id).ok_or_else(|| not_found(id))?;
            entry.record.session_user = user;
            Ok(entry.record.clone())
        })
    }

    fn kept_logins<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<KeptLogins, WorkspaceError>> {
        Box::pin(async move {
            self.get(id).await?;
            Ok(lock(&self.inner.state)
                .kept_logins
                .get(id.as_str())
                .cloned()
                .unwrap_or(KeptLogins {
                    logins: Vec::new(),
                    store_readable: true,
                }))
        })
    }

    fn forget_login<'a>(
        &'a self,
        id: &'a WorkspaceId,
        service: &'a str,
    ) -> BoxFuture<'a, Result<(), WorkspaceError>> {
        Box::pin(async move {
            self.get(id).await?;
            if !matches!(service, "claude" | "github" | "Claude Code" | "GitHub") {
                return Err(WorkspaceError::NotFound(format!(
                    "no login service {service:?}"
                )));
            }
            let mut state = lock(&self.inner.state);
            if state.login_store_refuses {
                return Err(WorkspaceError::Unavailable(
                    "the operating system's credential store did not delete a saved login; unlock it or start it and ask again (what was already deleted stays deleted)".to_owned(),
                ));
            }
            if let Some(kept) = state.kept_logins.get_mut(id.as_str()) {
                kept.logins
                    .retain(|login| login.service != service && login.name != service);
            }
            state
                .forgotten_logins
                .push((id.to_string(), service.to_owned()));
            Ok(())
        })
    }

    fn check_shell_init<'a>(
        &'a self,
        id: &'a WorkspaceId,
        script: &'a str,
    ) -> BoxFuture<'a, Result<ShellInitCheck, WorkspaceError>> {
        Box::pin(async move {
            let record = self.get(id).await?;
            if record.status != WorkspaceStatus::Running {
                return Ok(ShellInitCheck::NotRun(format!(
                    "{} is not running; Puddle checks the script when it starts",
                    record.name
                )));
            }
            let mut state = lock(&self.inner.state);
            state.shell_init_checked.push(script.to_owned());
            Ok(state
                .shell_init_check
                .clone()
                .unwrap_or(ShellInitCheck::Passed))
        })
    }

    fn attach<'a>(
        &'a self,
        id: &'a WorkspaceId,
        mode: AttachMode,
    ) -> BoxFuture<'a, Result<Attached, WorkspaceError>> {
        Box::pin(async move {
            let record = self.get(id).await?;
            if record.busy.is_some() || record.status != WorkspaceStatus::Running {
                return Err(WorkspaceError::Conflict(format!(
                    "{} is not running; start it first",
                    record.name
                )));
            }
            match mode {
                AttachMode::Browser => {
                    let target = BrowserTarget {
                        origin: format!(
                            "http://{}.localhost:{}",
                            record.id,
                            self.inner.front.port()
                        ),
                        front: self.inner.front,
                        settings: puddle_settings::resolve(
                            &puddle_settings::GlobalSettings::default(),
                            None::<&puddle_settings::WorkspaceSettings>,
                        ),
                        login: None,
                        open: None,
                    };
                    // The answer the host gives: the launcher opens a window or says why not.
                    Ok(
                        match self.inner.launcher.open_browser(&record, &target).await {
                            Ok(()) => Attached::opened(),
                            Err(err) => Attached::not_opened(err.to_string()),
                        },
                    )
                }
                AttachMode::Desktop => {
                    let target = DesktopTarget {
                        ssh_host: format!("puddle-{}", record.id),
                        path: format!("/workspaces/{}", record.id),
                        is_workspace_file: false,
                    };
                    Ok(
                        match self.inner.launcher.open_desktop(&record, &target).await {
                            Ok(()) => Attached::opened(),
                            Err(err) => Attached::not_opened(err.to_string()),
                        }
                        .with_open_uri(target.open_uri()),
                    )
                }
            }
        })
    }

    fn checkouts<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<Checkouts, WorkspaceError>> {
        Box::pin(async move {
            let now = self.inner.clock.now_ms();
            let mut state = lock(&self.inner.state);
            let entry = state.entries.get(id).ok_or_else(|| not_found(id))?;
            let (running, entries) = (
                entry.record.status == WorkspaceStatus::Running,
                entry.record.repositories.clone(),
            );
            if running {
                let found = state.guest.get(id).cloned().unwrap_or_default();
                state.last_listing.insert(id.clone(), (now, found));
            }
            let last = state.last_listing.get(id);
            let problem = (!running)
                .then(|| "the workspace is not running; start it to list its checkouts".to_owned());
            Ok(checkouts_view(
                running,
                last.map(|(at, _)| *at),
                problem,
                last.map(|(_, list)| list),
                &entries,
            ))
        })
    }

    fn add_repository<'a>(
        &'a self,
        id: &'a WorkspaceId,
        add: AddRepository,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            match add {
                AddRepository::Clone(new) => self.clone_into(id, new, true, git).await,
                AddRepository::Existing {
                    directory,
                    seen,
                    identity,
                    pull,
                    push,
                } => {
                    self.manage_existing(id, (&directory, &seen), identity, pull, push, git)
                        .await
                }
            }
        })
    }

    fn clone_again<'a>(
        &'a self,
        id: &'a WorkspaceId,
        entry_id: &'a EntryId,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let repository = {
                let state = lock(&self.inner.state);
                let entry = state.entries.get(id).ok_or_else(|| not_found(id))?;
                entry
                    .record
                    .repositories
                    .iter()
                    .find(|r| &r.entry == entry_id)
                    .cloned()
                    .ok_or_else(|| {
                        WorkspaceError::NotFound(format!("{entry_id} is not on the list"))
                    })?
            };
            let new = NewRepository {
                repository,
                identity: None,
                pull: true,
                push: true,
                local: None,
            };
            self.clone_into(id, new, false, git).await
        })
    }

    fn remove_repository<'a>(
        &'a self,
        id: &'a WorkspaceId,
        entry_id: &'a EntryId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = {
                let mut state = lock(&self.inner.state);
                let entry = state.entries.get_mut(id).ok_or_else(|| not_found(id))?;
                if let Some(busy @ (Operation::Creating | Operation::Deleting)) = entry.record.busy
                {
                    return Err(WorkspaceError::Conflict(format!(
                        "{} is {busy}; remove it from the list when that is done",
                        entry.record.label()
                    )));
                }
                let before = entry.record.repositories.len();
                entry.record.repositories.retain(|r| &r.entry != entry_id);
                if entry.record.repositories.len() == before {
                    return Err(WorkspaceError::NotFound(format!(
                        "{entry_id} is not on the list"
                    )));
                }
                entry.record.clone()
            };
            self.repositories_changed(&record);
            Ok(record)
        })
    }

    fn settings_changed(&self) -> BoxFuture<'_, ()> {
        self.inner
            .settings_changes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {})
    }
}

#[cfg(test)]
mod tests;
