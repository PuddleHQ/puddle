// SPDX-License-Identifier: GPL-3.0-or-later
//! The scenario file: what the fixture holds at start, and the named scripts a test can run.
//!
//! Everything a scenario names is applied through the same store calls the proxy and the API
//! use, so the data has the shape real data has (ids, audit records, rate limits).
//! Fields are all optional; an unknown field is an error, so a typo in a scenario file fails at
//! start instead of seeding nothing.

use std::collections::BTreeMap;

use puddle_api::wire::{
    CredentialSource, DoctorReport, FoundAccount, FoundVia, IdentityRequest, NetworkHealth,
    RepoListState, RepoNoteCode, RepoProblemCode, RepoRole, RepoVisibility, SignInStarted,
};
use puddle_types::Event;
use serde::{Deserialize, Serialize};

/// 2026-10-07T12:00:00Z. The fixture's clock starts here unless the scenario says otherwise; a
/// test installs the browser's fake clock at the same instant so relative times agree.
pub const DEFAULT_NOW_MS: u64 = 1_791_374_400_000;

const fn default_port() -> u16 {
    443
}

const fn one() -> u64 {
    1
}

/// A scenario: the state the fixture starts in and the scripts it can run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// A name for logs and `GET /control/state`.
    #[serde(default)]
    pub name: String,
    /// Epoch ms the clock starts at; [`DEFAULT_NOW_MS`] when left out.
    #[serde(default)]
    pub now_ms: Option<u64>,
    /// Rules, created first and in this order.
    #[serde(default)]
    pub rules: Vec<RuleSeed>,
    /// Rules on method and path, created after the rules and in this order.
    #[serde(default)]
    pub request_rules: Vec<RequestRuleSeed>,
    /// Requests workspaces made, decided in this order (a matching rule decides, else a pending
    /// row opens), each leaving its `connection` audit record.
    #[serde(default)]
    pub requests: Vec<RequestSeed>,
    /// Extra `connection` audit records (allowed traffic, blocks) that open nothing.
    #[serde(default)]
    pub connections: Vec<ConnectionSeed>,
    /// Workspaces that exist at start, with the state they are in.
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSeed>,
    /// Environment variables and secrets every workspace gets, created in this order.
    #[serde(default)]
    pub env: Vec<EnvSeed>,
    /// Identities that exist at start, in your order (the first one made is the default).
    #[serde(default)]
    pub identities: Vec<IdentityRequest>,
    /// What the credentials service answers: the accounts found, what is not signed in, the code
    /// a sign-in shows.
    #[serde(default)]
    pub credentials: CredentialsSeed,
    /// The repository lists the fake Git hosts answer with, one per credential of an identity.
    #[serde(default)]
    pub repo_lists: Vec<RepoListSeed>,
    /// What the fake Git hosts say about each credential's account (name, address, organisations);
    /// a credential left out answers that the host could not be reached.
    #[serde(default)]
    pub profiles: Vec<ProfileSeed>,
    /// How long the fake workspace service pauses between the steps of an operation (create,
    /// start, ...), in ms. Zero (the default) runs them back to back; tests that need to look
    /// at the busy state use the `hold_workspaces` step instead.
    #[serde(default)]
    pub workspace_step_delay_ms: u64,
    /// Settings documents kept as given.
    #[serde(default)]
    pub settings: SettingsSeed,
    /// What `GET /api/network-health` reports at start; a direct machine with no company roots
    /// when left out. `generated_at` is replaced by the clock on every request.
    #[serde(default)]
    pub network_health: Option<NetworkHealth>,
    /// What `GET /api/doctor` reports at start; a healthy machine when left out.
    #[serde(default)]
    pub doctor: Option<DoctorReport>,
    /// Leaves the first-run flow still to run. Left out, the scenario starts as an install the
    /// user has already been through, so the app opens on its start screen.
    #[serde(default)]
    pub first_run_open: bool,
    /// Named lists of steps; run one with `POST /control/script/{name}`.
    #[serde(default)]
    pub scripts: BTreeMap<String, Vec<Step>>,
}

impl Scenario {
    /// The clock's start.
    #[must_use]
    pub fn start_ms(&self) -> u64 {
        self.now_ms.unwrap_or(DEFAULT_NOW_MS)
    }
}

/// A background problem to raise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProblemSeed {
    /// Names the problem; raising the same key again replaces it.
    pub key: String,
    /// The workspace it is about; left out is a problem about Puddle.
    #[serde(default)]
    pub workspace: Option<String>,
    /// What went wrong, in one line.
    pub title: String,
    /// What Puddle did about it and what to do.
    pub detail: String,
}

/// A rule to create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSeed {
    /// `allow` or `deny`.
    pub effect: EffectSeed,
    /// `host`, `.suffix.example.com` or `*.suffix.example.com`.
    pub pattern: String,
    /// The one workspace it applies to; left out is every workspace.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Created this long before now (ms).
    #[serde(default)]
    pub ago_ms: u64,
    /// Expires this long after it was created (ms); left out is permanent.
    #[serde(default)]
    pub expires_in_ms: Option<u64>,
}

/// A rule on method and path to create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestRuleSeed {
    /// `host` or `*.suffix.example.com`.
    pub host: String,
    /// The methods it refuses; none for every method.
    #[serde(default)]
    pub methods: Vec<String>,
    /// The path pattern, such as `/repos/**`.
    pub path: String,
    /// The one workspace it applies to; left out is every workspace.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Created this long before now (ms).
    #[serde(default)]
    pub ago_ms: u64,
}

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectSeed {
    /// Let through.
    Allow,
    /// Refuse.
    Deny,
}

/// One repository of a seeded workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositorySeed {
    /// Where it is cloned from, an `https://` URL.
    pub url: String,
    /// Its checkout directory; the one the address gives when left out.
    #[serde(default)]
    pub directory: Option<String>,
}

/// A workspace that exists at start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSeed {
    /// The name (also the id).
    pub name: String,
    /// The repositories Puddle manages for it, in order; none when left out.
    #[serde(default)]
    pub repositories: Vec<RepositorySeed>,
    /// Its start folder, a path under the volume; the volume root when left out.
    #[serde(default)]
    pub start_folder: Option<String>,
    /// The image; the default image when left out.
    #[serde(default)]
    pub image: Option<String>,
    /// Memory in MiB; the default when left out.
    #[serde(default)]
    pub memory_mib: Option<u32>,
    /// Its state; `stopped` when left out.
    #[serde(default)]
    pub status: StatusSeed,
    /// Created this long before now (ms).
    #[serde(default)]
    pub ago_ms: u64,
    /// What the disk holds, in MiB.
    #[serde(default)]
    pub disk_used_mib: Option<u64>,
    /// What deleting it would lose; nothing when left out.
    #[serde(default)]
    pub unsaved: UnsavedSeed,
    /// Its identities and repository table; the defaults when left out.
    #[serde(default)]
    pub git: GitSeed,
    /// The checkouts its guest holds on the volume, as a listing of the running workspace finds
    /// them; none when left out. A listed repository whose directory is not here is "not on the
    /// disk".
    #[serde(default)]
    pub checkouts: Vec<CheckoutSeed>,
    /// How many more checkouts the guest holds than `checkouts` lists.
    #[serde(default)]
    pub checkouts_more: u64,
    /// Its own environment variables and secrets, created in this order.
    #[serde(default)]
    pub env: Vec<EnvSeed>,
    /// The shell init it has saved; none when left out.
    #[serde(default)]
    pub shell_init: Option<String>,
    /// The ports something in it listens on.
    #[serde(default)]
    pub listening_ports: Vec<u16>,
}

/// An environment variable or a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvSeed {
    /// The name.
    pub name: String,
    /// `plain` (the default) or `secret`.
    #[serde(default)]
    pub kind: EnvKindSeed,
    /// A plain variable's value, or a secret's, which the fixture keeps in memory and never shows.
    #[serde(default)]
    pub value: String,
    /// The hosts a secret's value may be sent to.
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// What an [`EnvSeed`] is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvKindSeed {
    /// A variable the workspace sees.
    #[default]
    Plain,
    /// A secret whose value stays outside the workspace.
    Secret,
}

/// One checkout the guest holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckoutSeed {
    /// Its directory under the volume root.
    pub directory: String,
    /// The address of its `origin` remote, an `https://` address; none when left out.
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Why there is no `remote_url` (or that credentials were removed from it).
    #[serde(default)]
    pub remote_note: Option<RemoteNoteSeed>,
    /// The branch; detached when left out.
    #[serde(default)]
    pub branch: Option<String>,
    /// The short commit; none (no commit yet) when left out.
    #[serde(default)]
    pub head: Option<String>,
    /// Uncommitted changes or untracked files.
    #[serde(default)]
    pub dirty: bool,
    /// Why Puddle could not read it.
    #[serde(default)]
    pub unreadable: Option<String>,
}

/// Why a checkout's remote address is not shown as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteNoteSeed {
    /// The address had a user name or password; they were removed.
    CredentialsRemoved,
    /// An SSH address.
    Ssh,
    /// Not an `https://` address.
    NotHttps,
    /// No remote.
    NoRemote,
}

const fn yes() -> bool {
    true
}

/// A workspace's Git settings at start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitSeed {
    /// The labels of its identities, in order.
    #[serde(default)]
    pub identities: Vec<String>,
    /// Its repository table.
    #[serde(default)]
    pub repos: Vec<GitRepoSeed>,
    /// "Only push to listed repos"; on when left out.
    #[serde(default)]
    pub only_push_listed: Option<bool>,
    /// "Only pull from listed repos"; off when left out.
    #[serde(default)]
    pub only_pull_listed: Option<bool>,
}

/// One row of a workspace's repository table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitRepoSeed {
    /// The host.
    pub host: String,
    /// The user or organisation.
    pub owner: String,
    /// The repository.
    pub repo: String,
    /// Pull on; on when left out.
    #[serde(default = "yes")]
    pub pull: bool,
    /// Push on; on when left out.
    #[serde(default = "yes")]
    pub push: bool,
}

/// What the credentials service answers at start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialsSeed {
    /// The accounts "found on this computer".
    #[serde(default)]
    pub found: Vec<FoundAccount>,
    /// Listings whose tool is not installed.
    #[serde(default)]
    pub missing: Vec<FoundVia>,
    /// Credentials that cannot be read: not signed in.
    #[serde(default)]
    pub signed_out: Vec<CredentialSource>,
    /// What a sign-in shows; a code and address when left out.
    #[serde(default)]
    pub sign_in: Option<SignInStarted>,
}

const fn ok_state() -> RepoListState {
    RepoListState::Ok
}

const fn read_role() -> RepoRole {
    RepoRole::Read
}

const fn private_visibility() -> RepoVisibility {
    RepoVisibility::Private
}

/// One credential's list of repositories (for an Azure DevOps credential, one organisation's),
/// as the host reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoListSeed {
    /// The label of the identity that holds the credential.
    pub identity: String,
    /// The credential's place in that identity's list (from 0).
    #[serde(default)]
    pub credential: usize,
    /// The credential's host.
    pub host: String,
    /// The Azure DevOps organisation; left out elsewhere.
    #[serde(default)]
    pub organisation: Option<String>,
    /// How current the list is; `ok` when left out.
    #[serde(default = "ok_state")]
    pub state: RepoListState,
    /// Read this long before now (ms); never read when left out.
    #[serde(default)]
    pub refreshed_ago_ms: Option<u64>,
    /// The host lets Puddle ask again this long from now (ms).
    #[serde(default)]
    pub retry_in_ms: Option<u64>,
    /// Why the newest read is missing or failed.
    #[serde(default)]
    pub problem: Option<RepoProblemSeed>,
    /// What the list leaves out.
    #[serde(default)]
    pub notes: Vec<RepoNoteSeed>,
    /// The repositories.
    #[serde(default)]
    pub repos: Vec<RepoItemSeed>,
}

/// What a Git host says about one credential's account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSeed {
    /// The credential it answers for (set this or `token`).
    #[serde(default)]
    pub source: Option<CredentialSource>,
    /// The pasted token it answers for, whatever id the fixture kept it under (set this or
    /// `source`). A token the UI keeps while the user types is only known by its value.
    #[serde(default)]
    pub token: Option<String>,
    /// The account name on the host.
    #[serde(default)]
    pub account: Option<String>,
    /// The display name.
    #[serde(default)]
    pub name: Option<String>,
    /// The address commits use.
    #[serde(default)]
    pub email: Option<String>,
    /// The organisations the account belongs to.
    #[serde(default)]
    pub organisations: Vec<String>,
    /// What is missing or limited.
    #[serde(default)]
    pub notes: Vec<RepoNoteSeed>,
    /// Why the profile could not be read.
    #[serde(default)]
    pub problem: Option<RepoProblemSeed>,
}

/// Why a list could not be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoProblemSeed {
    /// The class.
    pub code: RepoProblemCode,
    /// What happened and the way out.
    pub message: String,
    /// Whether signing in again is the way out.
    #[serde(default)]
    pub needs_sign_in: bool,
}

/// What a list leaves out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoNoteSeed {
    /// The class.
    pub code: RepoNoteCode,
    /// The note.
    pub message: String,
}

/// One repository in a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoItemSeed {
    /// The user or organisation that owns it; the organisation on Azure DevOps.
    pub owner: String,
    /// The Azure DevOps project; left out elsewhere.
    #[serde(default)]
    pub project: Option<String>,
    /// Its own name.
    pub name: String,
    /// The address to clone; built from the host, owner and name when left out.
    #[serde(default)]
    pub url: Option<String>,
    /// Who can see it; private when left out.
    #[serde(default = "private_visibility")]
    pub visibility: RepoVisibility,
    /// What the account can do in it; read when left out.
    #[serde(default = "read_role")]
    pub role: RepoRole,
    /// Archived.
    #[serde(default)]
    pub archived: bool,
    /// A fork.
    #[serde(default)]
    pub fork: bool,
}

/// A workspace's state at start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSeed {
    /// Created, never started.
    Created,
    /// Running.
    Running,
    /// Stopped.
    #[default]
    Stopped,
    /// Ended without a stop.
    Crashed,
    /// Its volume is gone.
    VolumeMissing,
}

/// What the delete check finds in a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnsavedSeed {
    /// Per checkout.
    #[serde(default)]
    pub repos: Vec<RepoSeed>,
    /// Data outside any checkout.
    #[serde(default)]
    pub other: Vec<String>,
    /// What the check could not read.
    #[serde(default)]
    pub errors: Vec<String>,
}

/// One checkout's findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSeed {
    /// The directory.
    pub dir: String,
    /// `git status --porcelain` lines.
    #[serde(default)]
    pub uncommitted: Vec<String>,
    /// `<hash> <subject>` lines.
    #[serde(default)]
    pub unpushed: Vec<String>,
    /// `git stash list` lines.
    #[serde(default)]
    pub stashes: Vec<String>,
}

/// A service a workspace keeps a login for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptLoginSeed {
    /// The service as forgetting names it (`claude`, `github`).
    pub service: String,
    /// What the user calls it (`Claude Code`).
    pub name: String,
    /// Epoch ms it was first noted as kept; unknown when left out.
    #[serde(default)]
    pub since: Option<u64>,
}

/// A long workspace operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOperationSeed {
    /// Create.
    Create,
    /// Start.
    Start,
    /// Stop.
    Stop,
    /// Reclaim space.
    Reclaim,
    /// Delete.
    Delete,
}

/// A workspace asking for `host:port`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSeed {
    /// The workspace's name.
    pub workspace: String,
    /// The host asked for.
    pub host: String,
    /// The port; 443 when left out.
    #[serde(default = "default_port")]
    pub port: u16,
    /// How many times it asked (the pending row counts attempts); 1 when left out.
    #[serde(default = "one")]
    pub repeat: u64,
    /// First asked this long before now (ms).
    #[serde(default)]
    pub ago_ms: u64,
}

/// One `connection` audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSeed {
    /// The workspace's name.
    pub workspace: String,
    /// The host.
    pub host: String,
    /// The port; 443 when left out.
    #[serde(default = "default_port")]
    pub port: u16,
    /// What happened.
    pub decision: DecisionSeed,
    /// Why, as the audit log's `reason` code: `rule` (default for allow and deny), `no_rule`
    /// (default for pending), `request_rule`, `puddle_endpoint`, `ssh_unsupported` or
    /// `local_address` (default for blocked).
    #[serde(default)]
    pub reason: Option<String>,
    /// The request the proxy read, and the rule on method and path that refused it (reason
    /// `request_rule`).
    #[serde(default)]
    pub request_line: Option<RequestLineSeed>,
    /// Bytes from the workspace.
    #[serde(default)]
    pub bytes_up: u64,
    /// Bytes to the workspace.
    #[serde(default)]
    pub bytes_down: u64,
    /// Recorded this long before now (ms).
    #[serde(default)]
    pub ago_ms: u64,
}

/// A request the proxy read in clear, and the rule on method and path that refused it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestLineSeed {
    /// `GET`, `DELETE`, ...
    pub method: String,
    /// The request target; only its path is kept.
    pub path: String,
    /// The id of the rule on method and path that refused it, when one did.
    #[serde(default)]
    pub rule: Option<i64>,
}

/// The audit log's connection decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSeed {
    /// Let through.
    Allow,
    /// Refused by a deny rule.
    Deny,
    /// Refused while waiting for the user.
    Pending,
    /// Refused whatever the rules say.
    Blocked,
}

/// Settings documents, as the API stores them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsSeed {
    /// The global document.
    #[serde(default)]
    pub global: Option<serde_json::Value>,
    /// Per-workspace documents by name.
    #[serde(default)]
    pub workspaces: BTreeMap<String, serde_json::Value>,
}

/// One thing a script (or `POST /control/step`) does. `{"do": "<name>", ...}` in JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Step {
    /// Puts an event on the SSE stream, as a component of Puddle would. `event` is the wire
    /// form: `{"type": "oom_kill", ...}`. New `Event` variants need no change here.
    Emit {
        /// The event.
        event: Event,
    },
    /// Moves the clock forward and sweeps what expired, as the sweeper would.
    Advance {
        /// Milliseconds.
        ms: u64,
    },
    /// Waits in real time, so a script can pace events for a page that is open.
    Wait {
        /// Milliseconds.
        ms: u64,
    },
    /// A workspace asks for a host.
    Request(RequestSeed),
    /// `count` workspaces-worth of requests to `n0.<domain>` .. `n<count-1>.<domain>`.
    Bulk {
        /// The workspace's name.
        workspace: String,
        /// How many hosts.
        count: u64,
        /// The shared domain; `bulk.example.org` when left out.
        #[serde(default)]
        domain: Option<String>,
    },
    /// `count` requests spread over workspaces `bulk-0`..`bulk-9` and domains `d0`..`d49` of
    /// `example.org` (the inbox's speed bar).
    Spread {
        /// How many requests.
        count: u64,
    },
    /// `count` connection records over the last 7 days, oldest first, across workspaces
    /// `bulk-0`..`bulk-9`, hosts `h<n>.d<m>.example.org` and the four decisions (the activity
    /// screen's speed bar).
    History {
        /// How many records.
        count: u64,
    },
    /// Holds every workspace operation before its next step, so the busy state stays put.
    HoldWorkspaces,
    /// Lets held workspace operations go on.
    ReleaseWorkspaces,
    /// Makes the next operation of this kind fail after its steps (once).
    FailWorkspace {
        /// Which operation.
        operation: WorkspaceOperationSeed,
        /// The reason the failed event carries.
        reason: String,
    },
    /// Makes forgetting a kept login fail (the credential store refuses) or work again, so a
    /// screen can show the way out the API gives.
    LoginStoreRefuses {
        /// Whether the store refuses.
        refuses: bool,
    },
    /// Sets the logins a workspace keeps, which its settings list shows; forgetting one removes it.
    KeptLogins {
        /// The workspace's name.
        workspace: String,
        /// The services kept.
        logins: Vec<KeptLoginSeed>,
        /// Whether the credential store answered; `false` shows the list as unknown. Defaults to
        /// `true`.
        #[serde(default = "yes")]
        store_readable: bool,
    },
    /// Replaces the network-health report and sends `network_changed` with its epoch, as a
    /// network or proxy change would.
    NetworkHealth(Box<NetworkHealth>),
    /// Makes a credential unreadable (not signed in) or readable again, as a sign-in or an
    /// expiry would.
    CredentialReadable {
        /// The credential's source.
        source: CredentialSource,
        /// Whether it reads now.
        readable: bool,
    },
    /// Replaces the accounts "found on this computer".
    CredentialsFound {
        /// The accounts.
        accounts: Vec<FoundAccount>,
    },
    /// Replaces the repository lists the fake Git hosts answer with.
    RepoLists {
        /// The lists.
        lists: Vec<RepoListSeed>,
    },
    /// Holds the answers of the repository lists, so a slow list stays slow.
    HoldRepos,
    /// Lets held repository list answers go.
    ReleaseRepos,
    /// Replaces what the system check reports from now on.
    Doctor(Box<DoctorReport>),
    /// Raises a background problem, as the host does for a clone that failed, or replaces the one
    /// with the same key. The list at `GET /api/problems` and `problems_changed` follow.
    Problem(ProblemSeed),
    /// Ends the background problem with this key, as a clone that works then would.
    ClearProblem {
        /// The problem's key.
        key: String,
    },
    /// Sets the ports something in a workspace listens on, as a program starting or stopping
    /// there would (the Ports tab reads the list again on `workspace_ports_changed`).
    Listening {
        /// The workspace's name.
        workspace: String,
        /// The ports that listen now.
        ports: Vec<u16>,
    },
    /// A tool in a workspace refused Puddle's certificate for a host, as the proxy finds out when
    /// a workspace decrypts every host: the host joins the workspace's learned list and
    /// `pass_through_learned` follows.
    Learned {
        /// The workspace's name.
        workspace: String,
        /// The host.
        host: String,
        /// Seconds the host stays passed through; 12 hours when left out.
        #[serde(default)]
        expires_in_secs: Option<u64>,
    },
    /// Makes the throwaway-shell check of a shell init refuse with this text (what the script
    /// printed), or pass again when it is left out. Only a running workspace is checked.
    ShellInitCheck {
        /// What the check says; passes when left out.
        #[serde(default)]
        refuses: Option<String>,
    },
    /// Creates a rule.
    Rule(RuleSeed),
    /// Writes a connection audit record.
    Connection(ConnectionSeed),
}
