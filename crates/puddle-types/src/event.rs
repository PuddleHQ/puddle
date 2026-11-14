// SPDX-License-Identifier: AGPL-3.0-or-later
//! Events Puddle reports to the user (API/SSE, tray, logs), and the sink they go into.

use std::fmt;
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::{CheckoutDir, WorkspaceId};

/// A workspace's state: its sandbox's state as the runtime reports it (msb's states, one for one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum WorkspaceStatus {
    /// Created but not started.
    Created,
    /// A start was accepted; not running yet.
    Starting,
    /// Running: exec and SSH work.
    Running,
    /// Shutting down gracefully.
    Draining,
    /// Paused.
    Paused,
    /// Stopped by an explicit stop.
    Stopped,
    /// Ended without an explicit stop: the VM died, or its owning handle was dropped.
    Crashed,
    /// The workspace is listed but its volume is gone, so it cannot start: restore the volume
    /// and start it again, or delete the workspace.
    VolumeMissing,
}

impl WorkspaceStatus {
    /// The `snake_case` name used on the wire and in messages.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Draining => "draining",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
            Self::Crashed => "crashed",
            Self::VolumeMissing => "volume_missing",
        }
    }

    /// Whether the workspace has no VM (it can be started or removed).
    #[must_use]
    pub fn is_down(self) -> bool {
        matches!(
            self,
            Self::Created | Self::Stopped | Self::Crashed | Self::VolumeMissing
        )
    }
}

impl fmt::Display for WorkspaceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Longest process name kept in [`Event::OomKill`] (the kernel's `TASK_COMM_LEN` is 16).
const MAX_PROCESS_NAME_CHARS: usize = 64;

/// Something the user should hear about. Serialised as one internally tagged enum
/// (`{"type":"oom_kill",...}`, ADR 0002).
///
/// Most events are about one workspace and carry a `workspace` field; global ones (crash report
/// waiting, consent needed, network change, ...) carry none, and [`Event::workspace`] returns
/// `None` for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum Event {
    /// A workspace changed state.
    StatusChanged {
        /// The workspace.
        workspace: WorkspaceId,
        /// Its new state.
        status: WorkspaceStatus,
    },
    /// The guest kernel's OOM killer ended a process (reported by the guest agent).
    ///
    /// `pid` and `process` come from the guest and are untrusted: build this variant with
    /// [`Event::oom_kill`], which bounds and cleans the name, and escape it when rendering.
    OomKill {
        /// The workspace whose guest killed the process.
        workspace: WorkspaceId,
        /// The killed process's id inside the guest.
        pid: u32,
        /// The killed process's name (`comm`), cleaned by [`Event::oom_kill`].
        process: String,
    },
    /// A disk of a workspace reached the warning threshold (`ui.disk_warning_percent`) or, with
    /// the workspace still over it, grew fuller. Said once when a disk crosses the threshold and
    /// again only after it has dropped below it.
    DiskNearlyFull {
        /// The workspace.
        workspace: WorkspaceId,
        /// Which disk.
        disk: DiskKind,
        /// How full it is, in whole percent (rounded down), 0 to 100.
        percent: u8,
    },
    /// A workspace operation (create, start, stop, reclaim, reset the root disk, delete) moved on. An operation ends
    /// with one event whose step is [`WorkspaceStep::Done`] or [`WorkspaceStep::Failed`].
    WorkspaceProgress {
        /// The workspace's workspace.
        workspace: WorkspaceId,
        /// What it is doing now.
        step: WorkspaceStep,
        /// More about the step, or why it failed; `null` when there is nothing to add. A cloning
        /// step names the address. It can quote tool output: escape it when rendering.
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(required = true))]
        detail: Option<String>,
        /// The checkout directory a cloning step is about; `null` for every other step.
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(required = true))]
        repository: Option<CheckoutDir>,
    },
    /// Puddle's install of a workspace's managed tools (mise) moved on. It runs in the background
    /// after a start, after a clone into the running workspace, and once more when the user allows
    /// a host the install was waiting on; the start never waits for it. Never sent while the layer is
    /// off.
    WorkspaceTools {
        /// The workspace.
        workspace: WorkspaceId,
        /// Where the install stands.
        state: ToolsState,
    },
    /// A new open pending request (a workspace asked for a host:port no rule decides).
    PendingOpened {
        /// The request, as `GET /api/pending` shows it.
        request: PendingSummary,
    },
    /// An open pending request was seen again.
    PendingUpdated {
        /// The workspace.
        workspace: WorkspaceId,
        /// The pending request's id.
        id: i64,
        /// Requests the row stands for now.
        attempts: u64,
        /// Epoch ms of the latest one.
        last_seen: u64,
    },
    /// A pending request is no longer open: decided by a user or by a rule, or expired.
    PendingClosed {
        /// The workspace.
        workspace: WorkspaceId,
        /// The pending request's id.
        id: i64,
        /// How it ended.
        state: PendingEnd,
        /// The rule that decided it; `null` for an expiry.
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(required = true))]
        rule_id: Option<i64>,
    },
    /// Rules were created, changed, deleted or expired. Global: every subscriber gets it (it
    /// carries nothing, so each refetches what it may see).
    RulesChanged {},
    /// A rule on method and path was created or deleted. Global: refetch `GET /api/request-rules`.
    RequestRulesChanged {},
    /// The list of identities, their order, the default or an identity's author or credentials
    /// changed, or the Git defaults. Carries no data: refetch `GET /api/identities` (every
    /// subscriber gets it, and each refetches its own).
    IdentitiesChanged {},
    /// A repository list Puddle renews in the background changed: a repository came or went, or
    /// the list could not be read, or a Git host started or stopped limiting requests. Carries no
    /// data: refetch `GET /api/repos`.
    RepoListsChanged {},
    /// A workspace's identities, repository table or "only listed" switches changed (also when an
    /// identity it has changed). Refetch `GET /api/workspaces/{id}/git`.
    WorkspaceGitChanged {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// A global environment (the variables and secrets every workspace of one user gets) changed.
    /// Carries no data: refetch `GET /api/env`, and a workspace's page refetches its own list too
    /// (every subscriber gets it, and each refetches its own).
    GlobalEnvChanged {},
    /// A workspace's own environment changed. Refetch `GET /api/workspaces/{id}/env`. Never carries
    /// a value.
    WorkspaceEnvChanged {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// A change to a workspace's environment could not be applied to it while it runs, or what a
    /// deleted workspace held could not be removed. `reason` names the variable and says what is
    /// wrong and what to do; it never holds a value. A workspace that runs keeps the secrets it
    /// had; the change applies at its next start, or when the next change succeeds.
    WorkspaceEnvFailed {
        /// The workspace.
        workspace: WorkspaceId,
        /// What went wrong, for the user.
        reason: String,
    },
    /// A workspace's shell init was saved or removed. Refetch `GET /api/workspaces/{id}/shell-init`.
    WorkspaceShellInitChanged {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// The workspace's own copy of its shell init (or the lines that load it) differed from the
    /// host's when it started, and Puddle put the host's back. Something inside the workspace
    /// changed it: tell the user.
    WorkspaceShellInitRestored {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// A workspace's direct SSH did not work: its endpoint could not be opened or stopped
    /// accepting, or a client's session ended with an error. `reason` says what failed and may
    /// quote the runtime's error. The workspace keeps running; an `ssh` client that was refused
    /// is told why on its own terminal.
    WorkspaceSshFailed {
        /// The workspace.
        workspace: WorkspaceId,
        /// What went wrong, for the user.
        reason: String,
    },
    /// A workspace's direct SSH accepts connections again after a [`Self::WorkspaceSshFailed`]
    /// that said it kept trying. The app withdraws that notice.
    WorkspaceSshRecovered {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// A workspace's repository list changed (an entry was added, managed, cloned again or
    /// removed). Carries nothing more: refetch `GET /api/workspaces/{id}` and its checkouts.
    WorkspaceRepositoriesChanged {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// The ports a workspace listens on, its forwards or its pinned forwards changed. Carries no
    /// data: refetch `GET /api/workspaces/{id}/ports`.
    WorkspacePortsChanged {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// A forward could not be set up or kept: a pinned forward that could not be restored at the
    /// workspace's start, or one whose port on this computer another program took. `reason` names
    /// the port and, when known, who has it; it may quote the operating system's error.
    WorkspacePortForwardFailed {
        /// The workspace.
        workspace: WorkspaceId,
        /// The port in the workspace.
        guest_port: u16,
        /// The port on this computer.
        host_port: u16,
        /// What went wrong and what to do, for the user.
        reason: String,
    },
    /// A tool in a workspace that decrypts every host refused Puddle's certificate for `host`, so
    /// Puddle stopped decrypting it and passes it through from now on (until the time is up or
    /// the user decrypts it again). Refetch `GET /api/workspaces/{id}/interception/learned`.
    PassThroughLearned {
        /// The workspace.
        workspace: WorkspaceId,
        /// The host, lower case.
        host: String,
        /// How many times Puddle has stopped decrypting this host in this workspace because a
        /// tool refused its certificate, this one included. The app raises a notice from the
        /// third time.
        count: u32,
    },
    /// A workspace asked for a credential and the source of it cannot supply one (not signed in,
    /// or the sign-in ran out). The user signs in from Puddle; a request never opens a sign-in
    /// window. Global: it goes to the owner's streams. Names only, never a value.
    CredentialSignInNeeded {
        /// The Git host the workspace was talking to.
        host: String,
        /// The source, as one line of names (`gh account me on github.com`).
        source: String,
    },
    /// A push or a fetch was refused because the workspace's repository table does not allow it.
    /// `host`, `owner` and `repo` are the table's spelling of the repository (lower-case, no
    /// `.git`), so the notice can add the row as it is.
    GitAccessDenied {
        /// The workspace that tried.
        workspace: WorkspaceId,
        /// The Git host.
        host: String,
        /// The user or organisation.
        owner: String,
        /// `repo`, or `project/repo` on Azure DevOps.
        repo: String,
        /// What was refused.
        access: GitAccess,
    },
    /// A Git host answered a workspace's request with `401` and no identity covers the owner, so
    /// Puddle added no credential and the server's answer reached the workspace as it is. Sent
    /// only when the workspace's own tool did not retry with a credential of its own soon after.
    /// `repo` is the table's spelling, as in [`Event::GitAccessDenied`].
    GitNoIdentity {
        /// The workspace that asked.
        workspace: WorkspaceId,
        /// The Git host.
        host: String,
        /// The user or organisation no identity covers.
        owner: String,
        /// `repo`, or `project/repo` on Azure DevOps.
        repo: String,
    },
    /// A login made inside a workspace could not be captured, or one Puddle kept could not be
    /// used (`kind` says which). The service is one of Puddle's built-in login profiles, never
    /// anything the guest sent. Names only, never a value.
    LoginProblem {
        /// The workspace the login is in.
        workspace: WorkspaceId,
        /// The service as the user knows it (`Claude Code`, `GitHub`).
        service: String,
        /// What is wrong.
        kind: LoginProblemKind,
    },
    /// A request of the workspace carried a credential Puddle did not add or swap, and the
    /// warning is on. The request went out as it was. `host` is the host it went to and `kind`
    /// says what Puddle could tell about the credential; never a part of its value.
    ForeignCredential {
        /// The workspace that sent it.
        workspace: WorkspaceId,
        /// The host the request went to.
        host: String,
        /// What kind of credential it looked like.
        kind: ForeignCredentialKind,
    },
    /// An answer to a request of the workspace held a token of a known kind that Puddle does not
    /// hold, and the warning is on. The answer went to the workspace as it was. `host` is the
    /// host that sent it and `kind` says what the token looked like; never a part of its value.
    UnknownTokenInAnswer {
        /// The workspace that got the answer.
        workspace: WorkspaceId,
        /// The host that answered.
        host: String,
        /// What kind of token it looked like.
        kind: UnknownTokenKind,
    },
    /// New audit records were committed. `id` is the newest record's id, so a client that holds
    /// everything up to `after` reads on with `GET /api/audit?after=`. One event per commit and
    /// user, not per record: it goes to the streams of the user the records belong to.
    AuditAppended {
        /// The newest audit record's id of that user.
        id: i64,
        /// The user the records belong to, as the store numbers users. Not sent: the stream is
        /// already that user's.
        #[serde(skip)]
        user: i64,
    },
    /// The network or the system's proxy settings changed (after a quiet period), so what
    /// `GET /api/network-health` showed may be out of date. Global: it goes to the owner's
    /// streams.
    NetworkChanged {
        /// The network epoch that began; it only grows while Puddle runs.
        epoch: u64,
    },
    /// Signing in to the company proxy failed (it turned the credentials or the token down, or
    /// asked again and again), so what `GET /api/network-health` lists under sign-in has changed.
    /// Sent once when a proxy goes from signed in (or never tried) to failing, not on every retry.
    /// Names the proxy and the scheme only, never a credential. Global: it goes to the owner's
    /// streams.
    ProxySignInFailed {
        /// The proxy as `host:port`.
        proxy: String,
        /// The scheme that was sent (`Negotiate`, `NTLM`, `Basic`); `null` when none was.
        #[serde(default)]
        #[cfg_attr(feature = "openapi", schema(required = true))]
        scheme: Option<String>,
    },
    /// The list of background problems changed (something Puddle does on its own failed, or the
    /// cause went away). Carries no data: refetch `GET /api/problems`. Global: every
    /// subscriber gets it (each refetches what it may see).
    ProblemsChanged {},
    /// The free space on the drive that holds Puddle's data moved across the warning or the
    /// failure line, or back above both (`level` is then `ok`). Sent when the level changes, not
    /// on every read; the first read after Puddle starts counts as a change when it is not `ok`.
    /// Global: it goes to the owner's streams.
    DiskSpaceChanged {
        /// Where the free space stands now.
        level: DiskSpaceLevel,
        /// The free space at the read that changed the level, in whole MiB.
        free_mib: u64,
    },
}

/// How the free space on the drive that holds Puddle's data compares with the warning and the
/// failure line (settings `ui.disk_warn_free_gb` and `ui.disk_fail_free_gb`). Ordered from fine to
/// worst, so `level > previous` says the space got tighter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DiskSpaceLevel {
    /// At or above the warning line.
    Ok,
    /// Under the warning line, at or above the failure line.
    Low,
    /// Under the failure line.
    Critical,
}

/// An open pending request as [`Event::PendingOpened`] carries it. `host` comes from the guest
/// (already normalised by the proxy): escape it when rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PendingSummary {
    /// The pending request's id.
    pub id: i64,
    /// The workspace that asked.
    pub workspace: WorkspaceId,
    /// The requested host, normalised.
    pub host: String,
    /// The host's registrable domain (`example.co.uk` for `api.example.co.uk`; the IP literal
    /// itself), the key the inbox groups rows by.
    pub registrable_domain: String,
    /// The requested port.
    pub port: u16,
    /// Epoch ms of the first request.
    pub first_seen: u64,
    /// Epoch ms of the latest one.
    pub last_seen: u64,
    /// Requests the row stands for.
    pub attempts: u64,
}

/// What a workspace's repository table refused, as [`Event::GitAccessDenied`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum GitAccess {
    /// A push (`git-receive-pack`).
    Push,
    /// A fetch or clone (`git-upload-pack`).
    Pull,
}

/// What is wrong with a login, as [`Event::LoginProblem`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum LoginProblemKind {
    /// The operating system's credential store did not take the token: the login failed and the
    /// workspace got nothing. Unlock the store or start it, then sign in again.
    StoreUnavailable,
    /// The service's answer was in a form Puddle does not read: the login failed and the workspace
    /// got nothing.
    UnexpectedAnswer,
    /// The token has a length or a character a stand-in cannot copy: the login failed and the
    /// workspace got nothing.
    UnusableToken,
    /// The service ties its tokens to a key of the tool's own, which a stand-in cannot stand in
    /// for: the login failed and the workspace got nothing.
    BoundToken,
    /// The answer carries a token in a field Puddle does not know, so it could not be kept: the
    /// login failed and the workspace got nothing.
    UnnamedToken,
    /// A login Puddle kept earlier could not be read back when the workspace started: the tool
    /// asks for a new sign-in.
    Unreadable,
    /// Login capture is off for the workspace but Puddle still holds logins it kept while capture
    /// was on, so the stand-ins in the workspace are not swapped and the services refuse them:
    /// turn capture on again, or sign in again.
    CaptureOff,
    /// A problem this version of Puddle has no name for. Nothing is known about it, so the notice
    /// says only that the login may not work.
    Unknown,
}

/// What Puddle can tell about a credential it did not add: by its bytes alone, never by whose it
/// is (the user's own token and a leaked one look the same). See [`Event::ForeignCredential`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum ForeignCredentialKind {
    /// A stand-in Puddle made once and no longer holds (a removed secret, a forgotten login, or
    /// another workspace's): it cannot be swapped, so the server will refuse it.
    StaleStandIn,
    /// A GitHub token with a valid checksum.
    GithubToken,
    /// An npm token with a valid checksum.
    NpmToken,
    /// A GitLab token with a valid checksum.
    GitlabToken,
    /// An Azure DevOps personal access token of the newer, 84-character kind.
    AzureDevopsToken,
    /// An `Authorization` header the workspace set itself on a request to a Git repository.
    GitAuthorization,
}

impl ForeignCredentialKind {
    /// The name the record and the API use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StaleStandIn => "stale_stand_in",
            Self::GithubToken => "github_token",
            Self::NpmToken => "npm_token",
            Self::GitlabToken => "gitlab_token",
            Self::AzureDevopsToken => "azure_devops_token",
            Self::GitAuthorization => "git_authorization",
        }
    }
}

/// What Puddle can tell about a token in an answer that it does not hold: by its bytes alone
/// (see [`Event::UnknownTokenInAnswer`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum UnknownTokenKind {
    /// A GitHub token with a valid checksum (`ghp_`, `gho_`, `ghu_`, `ghs_`, `ghr_`).
    GithubToken,
    /// A fine-grained GitHub token (`github_pat_`).
    GithubFineGrainedToken,
    /// A GitLab token (`glpat-`).
    GitlabToken,
    /// An npm token with a valid checksum.
    NpmToken,
    /// An Azure DevOps personal access token of the 84-character kind.
    AzureDevopsToken,
    /// An Anthropic API key or sign-in token (`sk-ant-`).
    AnthropicKey,
    /// An `OpenAI` project key (`sk-proj-`).
    OpenaiKey,
    /// A JSON Web Token in an answer that also has a `refresh_token` field.
    JwtWithRefreshToken,
}

impl UnknownTokenKind {
    /// The name the record and the API use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GithubToken => "github_token",
            Self::GithubFineGrainedToken => "github_fine_grained_token",
            Self::GitlabToken => "gitlab_token",
            Self::NpmToken => "npm_token",
            Self::AzureDevopsToken => "azure_devops_token",
            Self::AnthropicKey => "anthropic_key",
            Self::OpenaiKey => "openai_key",
            Self::JwtWithRefreshToken => "jwt_with_refresh_token",
        }
    }
}

/// How a pending request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PendingEnd {
    /// Allowed by a rule.
    Allowed,
    /// Denied by a rule.
    Denied,
    /// Closed without a decision because its workspace was deleted.
    Expired,
}

/// Which of a workspace's two disks an event or figure is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DiskKind {
    /// The sandbox's root disk: home directory, tool caches and installed packages. It goes with
    /// the sandbox and is made again by a reset.
    Root,
    /// The workspace volume, which holds the repositories and the user's work.
    Volume,
}

/// Where the install of a workspace's managed tools stands, as [`Event::WorkspaceTools`] reports
/// it. Tool names are `<name>@<version>` as mise reports them from the guest: escape them when
/// rendering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum ToolsState {
    /// Tools are being installed: `done` of `total` are in place, `tools` are the ones still
    /// missing.
    Installing {
        /// The tools still missing.
        tools: Vec<String>,
        /// How many of the missing ones are in place since the install began.
        done: u32,
        /// How many were missing when it began.
        total: u32,
    },
    /// Every tool the workspace's mise files ask for is installed.
    Ready,
    /// The install ended with tools still missing. The problem `tools:<workspace>` says why.
    Failed {
        /// The tools still missing.
        tools: Vec<String>,
        /// Hosts the install asked for that wait for a decision (pending requests opened or seen
        /// again while it ran). Puddle runs the install once more when the user allows one; empty
        /// when the failure had another cause.
        waiting_on: Vec<String>,
    },
}

/// Where a long workspace operation is, as [`Event::WorkspaceProgress`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum WorkspaceStep {
    /// Creating the workspace's disk volume.
    PreparingVolume,
    /// Pulling the workspace's image.
    PullingImage,
    /// Cloning the repository.
    Cloning,
    /// Booting the workspace's virtual machine.
    Starting,
    /// Bringing the checkouts in line with the volume after a boot.
    Syncing,
    /// Looking for work that is not on a remote.
    Checking,
    /// Giving freed disk space back to the host.
    Reclaiming,
    /// Removing the sandbox, and with it the root disk, so the next start makes a fresh one.
    ResettingRootDisk,
    /// Making the root disk bigger.
    GrowingRootDisk,
    /// Shutting the virtual machine down.
    Stopping,
    /// Removing the volume and the workspace.
    Removing,
    /// The operation finished.
    Done,
    /// The operation failed; `detail` says why.
    Failed,
}

impl Event {
    /// An [`Event::OomKill`], with `process` cut to 64 characters and control characters
    /// replaced by `?`, since it comes from the guest.
    ///
    /// ```
    /// use puddle_types::{Event, WorkspaceId};
    /// let e = Event::oom_kill(WorkspaceId::new("a").unwrap(), 42, "node\u{1b}[2J");
    /// assert!(matches!(e, Event::OomKill { ref process, .. } if process == "node?[2J"));
    /// ```
    #[must_use]
    pub fn oom_kill(workspace: WorkspaceId, pid: u32, process: &str) -> Self {
        let process = process
            .chars()
            .take(MAX_PROCESS_NAME_CHARS)
            .map(|c| if c.is_control() { '?' } else { c })
            .collect();
        Self::OomKill {
            workspace,
            pid,
            process,
        }
    }

    /// The workspace this event is about, or `None` for a global event.
    ///
    /// ```
    /// use puddle_types::{Event, WorkspaceId};
    /// let a = WorkspaceId::new("a").unwrap();
    /// assert_eq!(Event::oom_kill(a.clone(), 1, "x").workspace(), Some(&a));
    /// ```
    #[must_use]
    pub fn workspace(&self) -> Option<&WorkspaceId> {
        match self {
            Self::StatusChanged { workspace, .. }
            | Self::OomKill { workspace, .. }
            | Self::DiskNearlyFull { workspace, .. }
            | Self::WorkspaceProgress { workspace, .. }
            | Self::WorkspaceTools { workspace, .. }
            | Self::PendingUpdated { workspace, .. }
            | Self::PendingClosed { workspace, .. }
            | Self::GitAccessDenied { workspace, .. }
            | Self::GitNoIdentity { workspace, .. }
            | Self::LoginProblem { workspace, .. }
            | Self::ForeignCredential { workspace, .. }
            | Self::UnknownTokenInAnswer { workspace, .. }
            | Self::WorkspaceGitChanged { workspace }
            | Self::WorkspaceEnvChanged { workspace }
            | Self::WorkspaceEnvFailed { workspace, .. }
            | Self::WorkspaceShellInitChanged { workspace }
            | Self::WorkspaceShellInitRestored { workspace }
            | Self::WorkspaceRepositoriesChanged { workspace }
            | Self::WorkspaceSshFailed { workspace, .. }
            | Self::WorkspaceSshRecovered { workspace }
            | Self::WorkspacePortsChanged { workspace }
            | Self::WorkspacePortForwardFailed { workspace, .. }
            | Self::PassThroughLearned { workspace, .. } => Some(workspace),
            Self::PendingOpened { request } => Some(&request.workspace),
            Self::RulesChanged {}
            | Self::RequestRulesChanged {}
            | Self::IdentitiesChanged {}
            | Self::RepoListsChanged {}
            | Self::GlobalEnvChanged {}
            | Self::CredentialSignInNeeded { .. }
            | Self::AuditAppended { .. }
            | Self::NetworkChanged { .. }
            | Self::ProxySignInFailed { .. }
            | Self::ProblemsChanged {}
            | Self::DiskSpaceChanged { .. } => None,
        }
    }
}

/// Where events go. Implementations must not block: queue or drop, never wait on a consumer.
pub trait EventSink: Send + Sync {
    /// Hands over one event.
    fn emit(&self, event: Event);
}

/// A sink that drops every event.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: Event) {}
}

/// A sink that keeps every event in memory, for tests.
///
/// ```
/// use puddle_types::{CollectingSink, Event, EventSink, WorkspaceId};
/// let sink = CollectingSink::default();
/// sink.emit(Event::oom_kill(WorkspaceId::new("a").unwrap(), 1, "x"));
/// assert_eq!(sink.take().len(), 1);
/// assert!(sink.take().is_empty());
/// ```
#[derive(Debug, Default)]
pub struct CollectingSink {
    events: Mutex<Vec<Event>>,
}

impl CollectingSink {
    /// The events so far, leaving them in place.
    #[must_use]
    pub fn events(&self) -> Vec<Event> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The events so far, emptying the sink.
    #[must_use]
    pub fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl EventSink for CollectingSink {
    fn emit(&self, event: Event) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
}

impl<T: EventSink + ?Sized> EventSink for std::sync::Arc<T> {
    fn emit(&self, event: Event) {
        (**self).emit(event);
    }
}

#[cfg(test)]
mod tests;
