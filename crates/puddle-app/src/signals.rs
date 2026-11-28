// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quitting the app on a shutdown signal (`SIGINT`, `SIGTERM`, `SIGHUP`; on Windows Ctrl-C, Ctrl-Break
//! and logoff, system shutdown or console close).
//!
//! Tauri ends its loop on none of them, so without this the process ends the default way and
//! the VMs are neither trimmed nor stopped until the next start reconciles them.

use puddle_lifecycle::ShutdownSignals;

/// Waits for the first signal and calls `quit` (the app's own quit path, which stops the host), then
/// waits for a second one and calls `give_up`: a stop that hangs must not leave the user without a way out.
pub(crate) async fn quit_on_signals(
    mut signals: ShutdownSignals,
    quit: impl FnOnce(),
    give_up: impl FnOnce(),
) {
    let cause = signals.recv().await;
    tracing::info!(cause = cause.as_str(), "shutting down");
    quit();
    if let Err(cause) = signals.until_again(std::future::pending::<()>()).await {
        tracing::warn!(
            cause = cause.as_str(),
            "a second request to stop; leaving now"
        );
        give_up();
    }
}

#[cfg(all(test, unix))]
mod tests;
