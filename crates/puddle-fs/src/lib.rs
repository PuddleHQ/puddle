// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg_attr(not(windows), forbid(unsafe_code))]
//! The file-system seams that differ per OS:
//!
//! - [`data_dir`]: Puddle's per-user data folder, resolved by the `dirs` crate in this one place.
//! - [`private`]: owner-only files and folders. On Unix the modes `0600` / `0700`; on Windows an
//!   explicit, protected ACL with one entry, for the current user.
//! - [`allocated_bytes`]: what a sparse file (a VM's disk image) really takes on the host.
//! - [`lock`]: one host process per data folder, an OS lock that dies with its holder.
//!
//! | OS | `data_dir()` |
//! |---|---|
//! | Windows | `%LOCALAPPDATA%\puddle` (not roaming: it holds a VM disk and a token) |
//! | Linux | `$XDG_DATA_HOME/puddle`, else `~/.local/share/puddle` |
//! | macOS | `~/Library/Application Support/puddle` |

mod allocated;
mod data_dir;
pub mod lock;
pub mod private;
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Win32 calls for the user's SID, security descriptors and ACLs"
)]
pub mod win;

pub use allocated::allocated_bytes;
pub use data_dir::{DataDirError, data_dir, data_dir_in, user_ssh_config, user_ssh_config_in};
pub use lock::{DataLock, LockError};
