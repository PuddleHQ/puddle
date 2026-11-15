// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared types for Puddle's crates: identifiers, wire types and errors that cross crate
//! boundaries. Keep this crate small and free of I/O, so every other crate can depend on it.
//!
//! | Item | What |
//! |---|---|
//! | [`SandboxName`], [`WorkspaceId`], [`VolumeName`], [`ImageRef`], [`CheckoutDir`] | validated names; workspace id, sandbox and volume names are DNS labels |
//! | [`GuestPath`], [`GuestFile`], [`GuestEnv`] | what providers put into a guest; the boot hook applies them |
//! | [`ApplyKind`], [`MergeSpec`], [`unmerge`] | merged files: Puddle owns only some keys of a user's file; the engine `puddle-agent` runs in the guest |
//! | [`CpuCount`] | guest vCPU count: 1 to 32, default 1 (the setting is in `puddle-settings`) |
//! | [`MemoryMib`] | guest memory size: 256 MiB to 1 TiB, default 8 GiB (the setting is in `puddle-settings`) |
//! | [`Problem`], [`Problems`] | what Puddle did on its own that failed, listed until its cause goes away |
//! | [`WorkspaceStatus`], [`Event`], [`EventSink`] | workspace states and the user-facing event stream (incl. [`Event::OomKill`]) |
//! | [`Host`], [`DomainName`] | a normalised egress destination (the proxy normalises, everyone else validates) |
//! | [`EgressRequest`], [`Decision`], [`Policy`] | what the proxy asks the rules engine and what it gets back |
//! | [`ConnectionEvent`], [`ConnectionLog`] | one connection as the proxy reports it for the audit, and where it goes (the store) |
//! | [`QueryName`] | the name of a query parameter a secret is swapped in |
//! | [`CompanySignal`] | which host signal put an address in the company network (audit `signal`) |
//! | [`LocalCategory`] | the local-destination categories (spec R-14): one toggle each, classified by `puddle-netpolicy` |
//! | [`ValidationError`] | the one error every checked constructor returns |
//!
//! Every checked type validates in its constructor *and* when deserialised, so a value of the
//! type is always valid. Names are compared and hashed as plain strings.
#![forbid(unsafe_code)]

mod connection;
mod cpus;
mod error;
mod event;
mod fill;
#[cfg(any(test, feature = "testing"))]
pub mod fixture;
mod guest;
mod host;
mod interception;
mod local;
mod memory;
mod merge;
mod name;
pub mod panic_hook;
mod policy;
mod problems;
mod query_name;
mod request_match;
mod session_user;

pub use connection::{
    ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionOrigin, ConnectionOutcome,
    ConnectionReason, HttpRequestLine, NullConnectionLog, RefusedReason, request_path,
};
pub use cpus::CpuCount;
pub use error::ValidationError;
pub use event::DiskSpaceLevel;
pub use event::{
    CollectingSink, DiskKind, Event, EventSink, ForeignCredentialKind, GitAccess, LoginProblemKind,
    NullSink, PendingEnd, PendingSummary, ToolsState, UnknownTokenKind, WorkspaceStatus,
    WorkspaceStep,
};
pub use fill::{Fill, Rgb, SET_PALETTE};
pub use guest::{ApplyKind, FileMode, GuestEnv, GuestFile, GuestPath, GuestPathLit};
pub use host::{DomainName, Host, MAX_LABEL_LEN, MAX_NAME_LEN};
pub use interception::{CURATED_PASS_THROUGH, InterceptionMode};
pub use local::{CompanySignal, LocalCategory};
pub use memory::MemoryMib;
pub use merge::{
    MAX_MERGE_FILE, MergeEntry, MergeFormat, MergeRefusal, MergeSpec, Merged, Unmerged, unmerge,
};
pub use name::{
    CheckoutDir, EntryId, ImageRef, ImageRefLit, RESERVED_WORKSPACE_NAMES, SandboxName, VolumeName,
    WORKSPACE_VOLUME_PREFIX, WorkspaceId,
};

pub use policy::{
    BlockReason, Decision, EgressRequest, PatternKind, PendingId, PendingOutcome, Policy,
    PolicyError, ProtocolHint, RuleId, RuleSetId, SuffixAllows,
};
pub use problems::{Problem, Problems};
pub use query_name::{MAX_QUERY_NAME_LEN, QueryName};
pub use request_match::{
    CanonicalPath, MAX_METHODS, MAX_PATH_PATTERN, MAX_PATH_SEGMENTS, MethodError, Methods,
    PathPattern, PathPatternError, percent_decoded, target_path,
};
pub use session_user::SessionUser;

/// Puddle's version, from the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The line a Puddle binary prints for `--version`: the program name and Puddle's version.
///
/// ```
/// assert_eq!(puddle_types::version_line("puddle"), format!("puddle {}", puddle_types::VERSION));
/// ```
#[must_use]
pub fn version_line(program: &str) -> String {
    format!("{program} {VERSION}")
}

#[cfg(test)]
mod tests;
