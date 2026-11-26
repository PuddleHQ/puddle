// SPDX-License-Identifier: AGPL-3.0-or-later
//! The service traits Puddle's local API and the host share, the wire types they speak, and the
//! in-memory implementations the tests run on. `puddle-api` serves these over HTTP and
//! re-exports them; the host implements them.
#![forbid(unsafe_code)]

pub mod backup;
pub mod checkouts;
#[cfg(any(test, feature = "testing"))]
pub mod contract;
pub mod credential_store;
pub mod credentials;
pub mod dev_cert;
pub mod doctor;
pub mod error;
#[cfg(any(test, feature = "testing"))]
pub mod fake_workspaces;
pub mod learned;
pub mod network_health;
pub mod ports;
pub mod proxy_sign_in;
pub mod repos;
pub mod restore;
pub mod stored_ids;
pub mod terminals;
pub mod wire;
pub mod workspaces;

pub use backup::{
    BackupArchive, BackupService, BackupServiceError, FAKE_BACKUP_BYTES, FakeBackup, NoBackup,
    archive_file_name,
};
pub use checkouts::{
    AddRepository, Checkout, Checkouts, Found, FoundList, Manage, RemoteNote, check_addition,
    check_seen, checkouts_view, why_no_remote,
};
pub use credential_store::{CredentialStoreService, NoCredentialStore};
pub use credentials::{
    Check, CredentialService, CredentialsError, FakeCredentials, HostCredentials, NoCredentials,
    NoSecretStore,
};
pub use dev_cert::{DevCertService, FakeDevCert, NoDevCert};
pub use doctor::{DoctorError, DoctorService, FakeDoctor, HostDoctor, NoDoctor, RunChecks};
pub use error::{ApiError, ApiErrorBody, ErrorCode, ErrorField};
#[cfg(any(test, feature = "testing"))]
pub use fake_workspaces::{FakeLauncher, FakeWorkspaces, Unsaved};
pub use learned::{FakeLearned, LearnedService, NoLearned};
pub use network_health::{
    FakeNetworkHealth, HostNetworkHealth, NetworkHealthError, NetworkHealthService,
    NoNetworkHealth, PullProxyState, forward_network_changes, forward_proxy_sign_in_failures,
};
pub use ports::{FakePorts, NoPorts, PortsService, PortsServiceError};
pub use proxy_sign_in::{FakeProxySignIns, NoProxySignIns, ProxySignInError, ProxySignInService};
pub use repos::{FakeRepos, NoRepos, RepoService, ReposError};
pub use restore::{
    FAKE_RESTORE_MAGIC, FakeRestore, IdentityTokenRef, NoRestore, RestoreFacts, RestoreService,
    RestoreServiceError, SecretRef, UploadStream,
};
pub use terminals::{
    EndReason, FakeTerminals, MAX_IN_ALL, MAX_MESSAGE_BYTES, MAX_PER_WORKSPACE, NoTerminals,
    SUBPROTOCOL, TICKET_LIFETIME, TerminalEnd, TerminalError, TerminalInput, TerminalLink,
    TerminalOutput, TerminalService, TerminalSize, WINDOW_BYTES,
};
pub use workspaces::phase;
pub use workspaces::{
    AttachMode, Attached, BrowserTarget, CONNECTION_TOKEN_COOKIE, CheckoutProblem,
    DEFAULT_DISK_MIB, DEFAULT_IMAGE, DEFAULT_ROOT_DISK_MIB, DeleteCheck, DesktopTarget,
    DisplayName, DisplayNameError, EditorLogin, EditorSignIn, EntryProblem, GitSetup, IdDrawError,
    IdSource, KeptLogin, KeptLogins, LaunchError, Launcher, Listing, LocalCheckoutError,
    LocalSource, MAX_ID_DRAWS, MAX_REPO_URL_LEN, MAX_REPOSITORIES, NameIds, NewRepository,
    NewWorkspace, NoFreeEntryId, NoFreeId, NoGitSetup, NoWorkspaces, OpenTarget, Operation,
    RandomIds, RepoFindings, RepoUrl, RepoUrlError, Repository, RepositoryDraft,
    RepositoryListError, ShellInitCheck, StartFolder, StartFolderChange, StartFolderError,
    WorkspaceError, WorkspaceRecord, WorkspaceService, admit_delete_fingerprint, admit_grow_size,
    check_rename, check_repositories, check_repositories_with, check_session_user_change,
    check_start_folder_change, describe_image, entry_id_from_text, label_taken, new_entry_id,
    not_found,
};
