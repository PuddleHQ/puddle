// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading the host's certificate stores: `CryptoAPI` on Windows, nothing elsewhere (only Windows
//! has the admin- and user-added root stores Puddle syncs).

#[cfg(not(windows))]
use crate::store::{StoreError, StoreSnapshot, StoreSource};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
mod windows_trust;

#[cfg(windows)]
pub use windows_trust::WindowsTrust;

/// Reads `sources` into a snapshot (see [`crate::read_host_stores`]). Re-exported rather than
/// wrapped, so the one `read` a Linux run lists is the one that runs there.
#[cfg(windows)]
pub(crate) use windows::read;

/// No Windows stores on this host: an empty snapshot.
#[cfg(not(windows))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "same signature as the Windows reader"
)]
pub(crate) fn read(sources: &[StoreSource]) -> Result<StoreSnapshot, StoreError> {
    let _ = sources;
    Ok(StoreSnapshot::new())
}
