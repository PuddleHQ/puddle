// SPDX-License-Identifier: GPL-3.0-or-later
//! Shutdown and reconcile: Puddle's VMs live and die with Puddle.
//!
//! - **Shutdown.** [`Lifecycle`] holds the owning handle of every sandbox Puddle runs. When Puddle
//!   exits, [`Lifecycle::shutdown`] runs `fstrim` in each one (ADR 0006: msb mounts volumes without
//!   `discard`) and then stops it, so msb records it `Stopped`, not `Crashed`.
//! - **Shutdown triggers.** [`wait_for_shutdown`] resolves on Ctrl-C, Ctrl-Break, a closed
//!   console, logoff, system shutdown, `SIGINT`, `SIGTERM` or `SIGHUP`.
//! - **Hard kills.** On Windows every process Puddle starts, msb's VMM processes included, runs in
//!   one kill-on-close job object ([`job`]): if Puddle is killed (`TerminateProcess`, a crash), the
//!   job's last handle closes and Windows kills the VMs with it. This job is the one place where
//!   later hardening adds job limits. On Linux msb's own parent watchdog does the
//!   same.
//! - **Reconcile.** After a hard kill the records are wrong (`Running` for a VM that is gone, or
//!   `Crashed`). [`reconcile`] runs at start: it stops VMs a previous Puddle left running, removes
//!   records, stale directories and orphaned `ws-*` volumes Puddle no longer knows, and **never
//!   touches anything Puddle didn't create** (a foreign name). Leftover maintenance sandboxes
//!   (`m--*`) always go; [`adopt_workspaces`] then rebuilds the workspace holders.
//!
//! # Windows consoles: why Puddle runs as two processes
//!
//! Windows ends every process on a closed console, logoff or system shutdown about 5 s after the
//! event, and a graceful shutdown (`fstrim`, then stop, per VM) can take well over 5 s
//! ([`ShutdownConfig`]: up to 30 s trim and 45 s stop per VM). [`supervise`] avoids the
//! deadline: the process the user started (the
//! *front*) creates the job, then starts itself again as the *worker* with `CREATE_NO_WINDOW`,
//! on a hidden console of its own, so the worker outlives the front and finishes stopping the
//! VMs. The front relays the worker's output and turns console events into a shutdown request
//! on the worker's stdin. (msb's VMMs no longer share Puddle's console either: the msb fork starts
//! each with no console, in its own process group, so a keypress or a closed window doesn't
//! reach them. The deadline is why the split stays.)
//!
//! ```no_run
//! # fn run_puddle() -> i32 { 0 }
//! match puddle_lifecycle::supervise() {
//!     Ok(puddle_lifecycle::Role::Front { exit_code }) => std::process::exit(exit_code),
//!     Ok(puddle_lifecycle::Role::Worker) => std::process::exit(run_puddle()),
//!     Err(e) => panic!("{e}"),
//! }
//! ```
//!
//! Inside `run_puddle`: [`reconcile`] and [`adopt_workspaces`] first, then create or start
//! sandboxes and hand their owning handles to [`Lifecycle::manage`], then
//! `wait_for_shutdown().await` and [`Lifecycle::shutdown`].
#![cfg_attr(not(windows), forbid(unsafe_code))]

mod error;
pub mod job;
mod reconcile;
mod shutdown;
mod signal;
mod supervise;
mod trim;
#[cfg(windows)]
mod windows;

pub use error::LifecycleError;
pub use reconcile::{Failure, Inventory, ReconcileReport, adopt_workspaces, reconcile};
pub use shutdown::{Lifecycle, ShutdownConfig, ShutdownReport, StopOutcome, WorkspaceOutcome};
pub use signal::{ShutdownCause, ShutdownSignals, wait_for_shutdown};
pub use supervise::{ROLE_VAR, Role, supervise};
pub use trim::{TrimOutcome, trim_request};
