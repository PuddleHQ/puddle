// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's one error type.

use std::io;
use std::path::PathBuf;

/// What went wrong creating, binding or connecting to an endpoint.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IpcError {
    /// Something already exists at the endpoint's name. Puddle never shares a name with another
    /// process: the workspace must not boot against this endpoint.
    #[error(
        "endpoint {endpoint} already exists; another process may be squatting it, so Puddle refuses to use it"
    )]
    NameTaken {
        /// The endpoint that was taken.
        endpoint: PathBuf,
    },
    /// The endpoint was made by a different [`IpcRoot`](crate::IpcRoot).
    #[error("endpoint {endpoint} is not in the ipc root {root}")]
    ForeignEndpoint {
        /// The endpoint passed in.
        endpoint: PathBuf,
        /// The root it was passed to.
        root: PathBuf,
    },
    /// A name for a stable endpoint that is not a workspace-id-shaped label.
    #[error("{name:?} is not a valid endpoint name: use 1 to 63 characters of a-z, 0-9 and -")]
    InvalidName {
        /// The name that was refused.
        name: String,
    },
    /// A stable name was asked of a root that was not made with `IpcRoot::stable`.
    #[error("this ipc root has no stable names; make it with IpcRoot::stable")]
    NotStable,
    /// A Unix socket path is longer than the OS allows.
    #[error("socket path {path} is {len} bytes; the limit is {max}")]
    PathTooLong {
        /// The path.
        path: PathBuf,
        /// Its length in bytes.
        len: usize,
        /// The longest path the OS accepts.
        max: usize,
    },
    /// The root (directory, or the user's security identity on Windows) couldn't be set up.
    #[error("could not set up the ipc root {path}")]
    Root {
        /// The directory, or a description of what was being read.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: io::Error,
    },
    /// The system random source failed, so no unguessable name could be made.
    #[error("the system random source failed")]
    Random(#[source] getrandom::Error),
    /// Called outside a tokio runtime with I/O enabled.
    #[error("no tokio runtime: endpoints are bound and connected inside one")]
    NoRuntime,
    /// Nothing listens at the endpoint.
    #[error("nothing listens at {endpoint}")]
    NotFound {
        /// The endpoint.
        endpoint: PathBuf,
    },
    /// The endpoint refused this process.
    #[error("access to {endpoint} was denied")]
    AccessDenied {
        /// The endpoint.
        endpoint: PathBuf,
    },
    /// The server at the endpoint belongs to another user. Nothing was sent to it: the endpoint
    /// names are random per run, so a second user who took a freed name is not Puddle.
    #[error("the server at {endpoint} belongs to another user")]
    WrongOwner {
        /// The endpoint.
        endpoint: PathBuf,
    },
    /// Every pipe instance stayed busy for the whole connect timeout.
    #[error("every instance of {endpoint} stayed busy")]
    Busy {
        /// The endpoint.
        endpoint: PathBuf,
    },
    /// The listener can't accept any more connections.
    #[error("listener {endpoint} is closed")]
    Closed {
        /// The endpoint.
        endpoint: PathBuf,
    },
    /// Any other OS error.
    #[error("could not {op} {endpoint}")]
    Io {
        /// What was being done ("bind", "accept on", "connect to", ...).
        op: &'static str,
        /// The endpoint.
        endpoint: PathBuf,
        /// The OS error.
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests;
