// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspaces: list, create, detail, start, stop, reclaim space, the delete check, delete and
//! attach. The routes only translate; [`crate::WorkspaceService`] does the work. Long
//! operations answer 202 with the workspace marked busy and report through `status_changed` and
//! `workspace_progress` events.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use futures_util::future::BoxFuture;
use puddle_settings::{DiskWarningPercent, resolve};
use puddle_store::{IdentityId, RepoGitRequest, RepoRef, Store, UserId};
use puddle_types::WorkspaceId;

use crate::ApiErrorBody;
use crate::WorkspaceRecord;
use crate::authz::{self, Principal};
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::{AppState, repository_hosts};
use crate::wire::{
    AllowedHost, AttachRequest, AttachResponse, DeleteCheck, DeleteWorkspaceRequest,
    GrowRootDiskRequest, NewWorkspaceIdentity, NewWorkspaceRequest, RenameWorkspaceRequest,
    SessionUserRequest, Workspace, WorkspaceList, WorkspaceLookupConflict, WorkspaceMatch,
};
use crate::workspaces::{GitSetup, LocalSource, NewRepository, WorkspaceError};

/// The id in a path. A string that can't be an id names no workspace.
pub(super) fn workspace_id(raw: &str) -> Result<WorkspaceId, ApiError> {
    WorkspaceId::new(raw).map_err(|_| ApiError::not_found(format!("no workspace {raw:?}")))
}

/// Whether direct SSH is on for each workspace, in order. A settings document that cannot be
/// read counts as off: the answer fails closed, as the host's own check does.
/// Whether direct SSH is on for a workspace, and why that is only a guess when the settings
/// cannot be read.
struct DirectSsh {
    on: bool,
    /// Set when the settings could not be read: direct SSH then counts as off (the safe side).
    unreadable: Option<String>,
    /// The disk warning threshold of the global settings; Puddle's default when they cannot be read.
    disk_warning: DiskWarningPercent,
}

impl DirectSsh {
    fn unreadable(reason: &str) -> Self {
        Self {
            on: false,
            unreadable: Some(format!(
                "the settings cannot be read ({reason}); direct SSH counts as off and local destinations stay blocked until they can"
            )),
            disk_warning: DiskWarningPercent::DEFAULT,
        }
    }
}

/// `direct_ssh` of each of `records`, in order. Settings that cannot be read count as off and say
/// why, so a damaged document is not shown as "off" by the user's choice.
async fn direct_ssh_of(state: &AppState, records: &[WorkspaceRecord]) -> Vec<DirectSsh> {
    let names: Vec<_> = records.iter().map(|r| r.id.clone()).collect();
    let settings = state.settings.clone();
    let _lock = state.settings_lock.lock().await;
    let loaded = blocking(move || {
        let repo = settings.as_ref();
        let global = match crate::routes::settings::load_global(repo) {
            Ok(global) => global,
            Err(e) => return Ok(all_unreadable(names.len(), e.reason())),
        };
        let disk_warning = global.settings.ui.disk_warning_percent();
        Ok(names
            .iter()
            .map(
                |name| match crate::routes::settings::load_workspace(repo, name) {
                    Ok(own) => DirectSsh {
                        on: resolve(&global.settings, Some(&own.settings))
                            .direct_ssh
                            .value,
                        unreadable: None,
                        disk_warning,
                    },
                    Err(e) => DirectSsh::unreadable(e.reason()),
                },
            )
            .collect())
    })
    .await;
    match loaded {
        Ok(all) => all,
        Err(e) => all_unreadable(records.len(), e.reason()),
    }
}

fn all_unreadable(count: usize, reason: &str) -> Vec<DirectSsh> {
    (0..count).map(|_| DirectSsh::unreadable(reason)).collect()
}

/// `direct_ssh_of` for one record.
async fn direct_ssh_of_one(state: &AppState, record: &WorkspaceRecord) -> DirectSsh {
    let mut all = direct_ssh_of(state, std::slice::from_ref(record)).await;
    all.pop().unwrap_or(DirectSsh {
        on: false,
        unreadable: None,
        disk_warning: DiskWarningPercent::DEFAULT,
    })
}

/// Whether direct SSH is on for one workspace; the error says why it cannot be known.
async fn direct_ssh_on(state: &AppState, record: WorkspaceRecord) -> Result<bool, String> {
    let d = direct_ssh_of_one(state, &record).await;
    d.unreadable.map_or(Ok(d.on), Err)
}

/// The workspaces as the API shows them.
async fn views(state: &AppState, records: Vec<WorkspaceRecord>) -> Vec<Workspace> {
    let direct = direct_ssh_of(state, &records).await;
    records
        .into_iter()
        .zip(direct)
        .map(|(record, d)| present(record, d))
        .collect()
}

/// One workspace as the API shows it.
pub(super) async fn view(state: &AppState, record: WorkspaceRecord) -> Workspace {
    let d = direct_ssh_of_one(state, &record).await;
    present(record, d)
}

fn present(record: WorkspaceRecord, d: DirectSsh) -> Workspace {
    let warnings = record.disk_warnings(d.disk_warning);
    Workspace::new(record, d.on)
        .with_settings_error(d.unreadable)
        .with_disk_warnings(warnings)
}

/// Every workspace.
#[utoipa::path(
    get,
    path = "/api/workspaces",
    tag = "workspaces",
    responses((status = OK, description = "the workspaces", body = WorkspaceList))
)]
pub(crate) async fn list_workspaces(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<WorkspaceList>, ApiError> {
    let workspaces = own(&state, principal, state.workspaces.list().await?).await?;
    Ok(Json(WorkspaceList {
        workspaces: views(&state, workspaces).await,
    }))
}

/// The workspaces of `records` that are `principal`'s.
async fn own(
    state: &AppState,
    principal: Principal,
    mut records: Vec<WorkspaceRecord>,
) -> Result<Vec<WorkspaceRecord>, ApiError> {
    let owners = authz::owners(state).await?;
    records.retain(|r| owners.is(&r.id, principal.user));
    Ok(records)
}

/// What `GET /api/workspaces/lookup` takes.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LookupQuery {
    q: String,
}

/// Finds a workspace by text a person typed: its id exactly, or its name ignoring case. For a
/// command line or a script; every other route takes the id.
#[utoipa::path(
    get,
    path = "/api/workspaces/lookup",
    tag = "workspaces",
    params(("q" = String, Query, description = "an id, or a name ignoring case")),
    responses(
        (status = OK, description = "the one workspace it names", body = WorkspaceMatch),
        (status = NOT_FOUND, description = "no workspace has that id or name", body = ApiErrorBody),
        (status = CONFLICT, description = "the text is one workspace's id and another's name; ask for the id", body = WorkspaceLookupConflict),
        (status = UNPROCESSABLE_ENTITY, description = "`q` is empty", body = ApiErrorBody)
    )
)]
pub(crate) async fn lookup_workspace(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Query(query): crate::extract::Query<LookupQuery>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    if query.q.trim().is_empty() {
        return Err(ApiError::invalid("q must not be empty"));
    }
    let records = own(&state, principal, state.workspaces.list().await?).await?;
    let found = crate::workspaces::lookup(&records, &query.q);
    let matches: Vec<WorkspaceMatch> = found
        .iter()
        .map(|r| WorkspaceMatch {
            id: r.id.to_string(),
            name: r.name.to_string(),
        })
        .collect();
    match matches.as_slice() {
        [] => Err(ApiError::not_found(format!(
            "no workspace has the id or name {:?}",
            query.q
        ))),
        [one] => Ok(Json(one.clone()).into_response()),
        _ => {
            let both = matches
                .iter()
                .map(|m| format!("{} (id {})", m.name, m.id))
                .collect::<Vec<_>>()
                .join(" and ");
            let body = WorkspaceLookupConflict {
                error: crate::ErrorCode::Conflict,
                message: format!("{:?} names two workspaces: {both}; use the id", query.q),
                matches,
            };
            Ok((StatusCode::CONFLICT, Json(body)).into_response())
        }
    }
}

/// What the store asks of one repository's Git settings; `None` when its address names no
/// repository the table can hold.
pub(super) fn git_request(repository: &NewRepository) -> Option<RepoGitRequest> {
    let repo = RepoRef::from_https_url(repository.repository.url.as_str()).ok()?;
    Some(RepoGitRequest {
        repo,
        identity: repository.identity,
        pull: repository.pull,
        push: repository.push,
    })
}

/// Sets a new workspace's repositories up in the store, in order and in one transaction, and
/// keeps what each got for the answer. A repository it cannot set up is cloned without its Git
/// settings, and its entry in the answer says so; the workspace is not undone for it.
pub(super) struct StoreGitSetup {
    store: Arc<Store>,
    /// Whether it also allows the hosts of the repositories no rule decides yet (a create, or
    /// adding a repository; not a clone again or an adopted volume).
    allow_hosts: bool,
    started: Mutex<Vec<NewWorkspaceIdentity>>,
    allowed: Mutex<Vec<AllowedHost>>,
}

impl StoreGitSetup {
    pub(super) fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            allow_hosts: false,
            started: Mutex::new(Vec::new()),
            allowed: Mutex::new(Vec::new()),
        }
    }

    /// Sets up as [`Self::new`] does and, before the first clone, allows each repository's host
    /// for the workspace unless a rule decides it already (ST-44a).
    pub(super) fn allowing_hosts(store: Arc<Store>) -> Self {
        Self {
            allow_hosts: true,
            ..Self::new(store)
        }
    }

    pub(super) fn take_allowed(&self) -> Vec<AllowedHost> {
        std::mem::take(&mut *self.allowed.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub(super) fn take(&self) -> Vec<NewWorkspaceIdentity> {
        std::mem::take(&mut *self.started.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// What the answer says about a repository whose Git settings were not set up.
fn not_set_up(url: &str, why: Option<&puddle_store::StoreError>) -> NewWorkspaceIdentity {
    NewWorkspaceIdentity::not_set_up(match why {
        None => format!(
            "Puddle cannot tell which repository {url} is, so it set up no identity or repository \
             table row for it. Add them on the Git tab."
        ),
        Some(err) => format!(
            "Puddle could not set up the Git settings of {url} ({err}), so it is cloned without \
             them. Set them on the Git tab."
        ),
    })
}

/// Sets up the whole list in one store transaction, so the workspace's identities and table rows
/// are all there or none is. A list the store refuses (a race with an identity deleted since the
/// check) is cloned without any Git settings, and every entry says so.
fn start_all(
    store: &Store,
    workspace: &WorkspaceId,
    repositories: &[NewRepository],
) -> Vec<NewWorkspaceIdentity> {
    let requests: Vec<Option<RepoGitRequest>> = repositories.iter().map(git_request).collect();
    let asked: Vec<RepoGitRequest> = requests.iter().flatten().cloned().collect();
    let started = match store.start_repositories_git(workspace, &asked) {
        Ok(started) => started,
        Err(err) => {
            tracing::warn!(%workspace, error = ?err, "could not set up a new workspace's Git settings");
            return repositories
                .iter()
                .map(|r| not_set_up(r.repository.url.as_str(), Some(&err)))
                .collect();
        }
    };
    let mut started = started.iter();
    repositories
        .iter()
        .zip(&requests)
        .map(|(repository, request)| {
            match request.as_ref().and_then(|r| Some((r, started.next()?))) {
                Some((request, start)) => NewWorkspaceIdentity::from_store(start, &request.repo),
                None => not_set_up(repository.repository.url.as_str(), None),
            }
        })
        .collect()
}

/// The grant failed: the clone waits in the inbox as it did before the create allowed hosts.
fn warn_not_allowed(workspace: &WorkspaceId, err: &puddle_store::StoreError) {
    tracing::warn!(%workspace, error = ?err, "could not allow the hosts of a new workspace's repositories");
}

impl GitSetup for StoreGitSetup {
    fn claim(&self, workspace: &WorkspaceId, owner: UserId) -> Result<(), WorkspaceError> {
        self.store
            .set_workspace_owner(workspace, owner)
            .map_err(|err| match err {
                puddle_store::StoreError::WorkspaceOwned(_) => {
                    WorkspaceError::Conflict(format!("the workspace id {workspace} is taken"))
                }
                err => WorkspaceError::Unavailable(format!(
                    "whose the new workspace is could not be saved ({err}); nothing was created"
                )),
            })
    }

    fn set_up<'a>(
        &'a self,
        workspace: &'a WorkspaceId,
        repositories: &'a [NewRepository],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            // On this thread, not a blocking task: a few small writes, and a task that could not
            // run would leave repositories set up without the answer saying which.
            let started = start_all(&self.store, workspace, repositories);
            *self.started.lock().unwrap_or_else(PoisonError::into_inner) = started;
            if self.allow_hosts {
                let hosts = repository_hosts::hosts_of(
                    repositories.iter().map(|r| r.repository.url.as_str()),
                );
                let allowed = self
                    .store
                    .workspace_owner(workspace)
                    .and_then(|owner| {
                        owner.ok_or_else(|| {
                            puddle_store::StoreError::UnknownWorkspace(workspace.clone())
                        })
                    })
                    .and_then(|owner| {
                        self.store
                            .as_user(owner)
                            .allow_hosts_for_create(workspace, &hosts)
                    });
                match allowed {
                    Ok(made) => {
                        *self.allowed.lock().unwrap_or_else(PoisonError::into_inner) = made
                            .into_iter()
                            .map(|rule| AllowedHost {
                                host: rule.pattern.to_string(),
                                rule_id: rule.id.0,
                            })
                            .collect();
                    }
                    // The clone then waits in the inbox as it did before the create allowed
                    // hosts; the workspace is not undone for it.
                    Err(err) => warn_not_allowed(workspace, &err),
                }
            }
        })
    }
}

/// Creates a workspace: its disk, then a clone of each repository in the list, one at a time.
/// The list is checked whole before anything exists. Each repository's identity joins the
/// workspace and its repository table row is made before the first clone. A clone that fails
/// fails only its repository: a background problem names it and the create still ends `done`.
/// Puddle picks the new workspace's id (eight random characters, never one used before); the
/// answer carries it. Answers 202; follow it with `workspace_progress` events.
#[utoipa::path(
    post,
    path = "/api/workspaces",
    tag = "workspaces",
    request_body = NewWorkspaceRequest,
    responses(
        (status = ACCEPTED, description = "creation started", body = Workspace),
        (status = CONFLICT, description = "another workspace has that name (ignoring case), two identities of the list cover the same place (`identity_collision`), or a block rule covers the host of a repository (the create never overrides a block)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "invalid name, repository (address, SSH remote, directory clash, the same repository twice, unknown identity, more than 100), start folder, image or memory, or `repo_url` sent", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "workspaces are not available, or Puddle could not pick a new id (16 draws were all taken: a broken random source)", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_workspace(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Json(body): crate::extract::Json<NewWorkspaceRequest>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let store = state.store.clone();
    let user = principal.user;
    let known: BTreeSet<IdentityId> =
        blocking(move || Ok(store.identities(user)?.into_iter().map(|i| i.id).collect())).await?;
    let mut paths = body.local_paths();
    // Looked at only for a list the create can take: each look reads the disk.
    if body.repositories.len() > crate::workspaces::MAX_REPOSITORIES {
        paths.clear();
    }
    paths.sort();
    paths.dedup();
    let service = state.workspaces.clone();
    let locals: BTreeMap<String, Result<LocalSource, String>> = blocking(move || {
        Ok(paths
            .into_iter()
            .map(|p| {
                let found = service.local_checkout(&p);
                (p, found)
            })
            .collect())
    })
    .await?;
    let new = body.into_new(user, |id| known.contains(&id), &locals)?;
    let store = state.store.clone();
    let requests: Vec<RepoGitRequest> = new.repositories.iter().filter_map(git_request).collect();
    blocking(move || Ok(store.check_repository_git(user, None, &requests)?)).await?;
    let hosts =
        repository_hosts::hosts_of(new.repositories.iter().map(|r| r.repository.url.as_str()));
    repository_hosts::refuse_blocked(&state.store, user, None, hosts).await?;
    let git = StoreGitSetup::allowing_hosts(state.store.clone());
    let record = state.workspaces.create(new, &git).await?;
    // A new workspace may start with direct SSH on (the global default).
    crate::system_managed::refresh(&state).await;
    Ok((
        StatusCode::ACCEPTED,
        Json(
            view(&state, record)
                .await
                .with_identities(git.take())
                .with_allowed_hosts(git.take_allowed()),
        ),
    ))
}

/// One workspace.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the workspace", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Workspace>, ApiError> {
    let record = state.workspaces.get(&workspace_id(&id)?).await?;
    Ok(Json(view(&state, record).await))
}

/// Renames a workspace: sets the name the UI shows. Only the name changes (the id, the volume
/// and the sandbox stay), so it works on a running workspace and restarts
/// nothing. A name another workspace already goes by (ignoring case) is refused with 409, and so
/// is a rename while the workspace is being created or deleted.
#[utoipa::path(
    patch,
    path = "/api/workspaces/{id}",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    request_body = RenameWorkspaceRequest,
    responses(
        (status = OK, description = "the renamed workspace", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "another workspace has that name, or this one is being created or deleted", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "empty, too long or contains control characters", body = ApiErrorBody)
    )
)]
pub(crate) async fn rename_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<RenameWorkspaceRequest>,
) -> Result<Json<Workspace>, ApiError> {
    let id = workspace_id(&id)?;
    let record = state.workspaces.rename(&id, body.into_name()?).await?;
    Ok(Json(view(&state, record).await))
}

/// Sets the guest user the workspace's sessions, browser editor and clones run as: `agent`
/// (passwordless `sudo`) or `root`. It applies at the next start; a running workspace keeps the
/// user it started with until then. A workspace that is being created or deleted answers 409.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/session-user",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    request_body = SessionUserRequest,
    responses(
        (status = OK, description = "the workspace with its new session user", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is being created or deleted", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "not `agent` or `root`", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_session_user(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<SessionUserRequest>,
) -> Result<Json<Workspace>, ApiError> {
    let id = workspace_id(&id)?;
    let record = state
        .workspaces
        .set_session_user(&id, body.session_user)
        .await?;
    Ok(Json(view(&state, record).await))
}

/// Starts a workspace that is down. Answers 202.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/start",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = ACCEPTED, description = "start accepted", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "already running or busy", body = ApiErrorBody)
    )
)]
pub(crate) async fn start_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let record = state.workspaces.start(&workspace_id(&id)?).await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, record).await)))
}

/// Stops a running workspace. Answers 202.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/stop",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = ACCEPTED, description = "stop accepted", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "not running or busy", body = ApiErrorBody)
    )
)]
pub(crate) async fn stop_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let record = state.workspaces.stop(&workspace_id(&id)?).await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, record).await)))
}

/// Gives the disk space the workspace freed back to the host. Answers 202.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/reclaim",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = ACCEPTED, description = "reclaim accepted", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "busy", body = ApiErrorBody)
    )
)]
pub(crate) async fn reclaim_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let record = state.workspaces.reclaim(&workspace_id(&id)?).await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, record).await)))
}

/// Removes the stopped workspace's sandbox and with it the root disk, so the next start makes a
/// fresh one: what was installed or cached outside the volume is lost, the volume is untouched.
/// Nothing runs by itself: this is the user's choice. Answers 202.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/reset-root-disk",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = ACCEPTED, description = "reset accepted", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "busy, or not stopped", body = ApiErrorBody)
    )
)]
pub(crate) async fn reset_root_disk(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let record = state
        .workspaces
        .reset_root_disk(&workspace_id(&id)?)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, record).await)))
}

/// Makes the workspace's root disk bigger. The root disk holds the home directory, tool caches
/// and installed packages, and only ever grows. A running workspace has the room at once when
/// the runtime can grow a disk under a running guest; otherwise Puddle restarts it, which loses
/// nothing that was saved to disk. A stopped workspace has the room at its next start. The size
/// is kept when the root disk is reset. Answers 202; the workspace is busy
/// (`growing_root_disk`) until it is done.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/grow-root-disk",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    request_body = GrowRootDiskRequest,
    responses(
        (status = ACCEPTED, description = "growing started", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "busy, or starting or stopping", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "not above the current size, or more than this computer has free", body = ApiErrorBody)
    )
)]
pub(crate) async fn grow_root_disk(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<GrowRootDiskRequest>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let record = state
        .workspaces
        .grow_root_disk(&workspace_id(&id)?, body.size_mib)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, record).await)))
}

/// What deleting the workspace would lose: uncommitted changes, unpushed commits, stashes and
/// data outside any checkout. Can take a while (it looks inside the workspace).
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/delete-check",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the report", body = DeleteCheck),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_check(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<DeleteCheck>, ApiError> {
    Ok(Json(
        state
            .workspaces
            .delete_check(&workspace_id(&id)?)
            .await?
            .into(),
    ))
}

/// Deletes a stopped workspace, its disk included. The body must confirm. The service checks
/// again first and refuses (409) unless the fresh check has the `fingerprint` the user saw, or
/// is clean when none is sent. Answers 202.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    request_body = DeleteWorkspaceRequest,
    responses(
        (status = ACCEPTED, description = "delete accepted", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "running, busy, or changed since it was checked", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "not confirmed", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<DeleteWorkspaceRequest>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let id = workspace_id(&id)?;
    if !body.confirm {
        return Err(ApiError::invalid(
            "deleting a workspace needs `confirm: true`",
        ));
    }
    let record = state
        .workspaces
        .delete(&id, body.fingerprint.as_deref())
        .await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, record).await)))
}

/// Opens a running workspace in VS Code: on the desktop (Puddle opens it) or in the browser
/// (in a window Puddle opens).
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/attach",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    request_body = AttachRequest,
    responses(
        (status = OK, description = "what was done", body = AttachResponse),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "not running, or direct SSH is off for a desktop attach", body = ApiErrorBody)
    )
)]
pub(crate) async fn attach_workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<AttachRequest>,
) -> Result<Json<AttachResponse>, ApiError> {
    let id = workspace_id(&id)?;
    let mode = crate::AttachMode::from(body.mode);
    if mode == crate::AttachMode::Desktop {
        // Desktop editors connect over SSH, which Puddle only opens when the user allowed it.
        // A workspace that is not up gets the service's own "start it first" answer.
        let record = state.workspaces.get(&id).await?;
        let up = record.busy.is_none() && record.status == puddle_types::WorkspaceStatus::Running;
        if up {
            match direct_ssh_on(&state, record).await {
                Ok(true) => {}
                Ok(false) => {
                    return Err(WorkspaceError::Conflict(
                        "direct SSH is off for this workspace; allow it first".to_owned(),
                    )
                    .into());
                }
                Err(reason) => return Err(WorkspaceError::Conflict(reason).into()),
            }
        }
    }
    let attached = state.workspaces.attach(&id, mode).await?;
    Ok(Json(attached.into()))
}

#[cfg(test)]
mod tests;
