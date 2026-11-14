// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether the user that runs Puddle has a graphical login session, asked of `loginctl`.
//!
//! Puddle reads this for one warning only: a start without the headless flag on a machine that
//! has no graphical session probably has no unlocked keyring either. It never decides which
//! store holds the secrets.

use std::time::Duration;

use crate::error::Tool;
use crate::run::{TOOL_TIMEOUT, ToolPaths, run_with_input};

/// What the user's login sessions say about a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginSession {
    /// The user's display session is of type `x11`, `wayland` or `mir`.
    Graphical,
    /// The user has no display session, or it is a text login (`tty`, `unspecified`, `web`).
    NotGraphical,
    /// Not asked or not answered: another platform, no `loginctl`, no systemd, a timeout.
    Unknown,
}

/// Reads the display session of the user this process runs as, with `loginctl show-user` for its
/// `Display` session and `loginctl show-session` for that session's `Type` (the answers of
/// `org.freedesktop.login1`; `Display` alone is not enough, on a WSL host it names a `tty` session).
pub async fn login_session(tools: &ToolPaths) -> LoginSession {
    match own_uid() {
        Some(uid) => login_session_of(tools, uid, TOOL_TIMEOUT).await,
        None => LoginSession::Unknown,
    }
}

/// The user id of this process, where `loginctl` is asked about it (Linux).
#[must_use]
pub fn login_user_id() -> Option<u32> {
    own_uid()
}

#[cfg(target_os = "linux")]
fn own_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(target_os = "linux"))]
fn own_uid() -> Option<u32> {
    None
}

/// [`login_session`] for user `uid`, with a limit for each call.
async fn login_session_of(tools: &ToolPaths, uid: u32, timeout: Duration) -> LoginSession {
    let Ok(program) = tools.get(Tool::Loginctl) else {
        return LoginSession::Unknown;
    };
    let ask = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        async move {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let output = run_with_input(Tool::Loginctl, program, &args, None, timeout)
                .await
                .ok()?;
            output
                .success
                .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        }
    };
    let uid = uid.to_string();
    let Some(display) = ask(&["show-user", &uid, "--property=Display", "--value"]).await else {
        return LoginSession::Unknown;
    };
    if display.is_empty() {
        return LoginSession::NotGraphical;
    }
    // The id is passed back to `loginctl`: only what a session id looks like goes through.
    if !display.chars().all(|c| c.is_ascii_alphanumeric()) {
        return LoginSession::Unknown;
    }
    let Some(kind) = ask(&["show-session", &display, "--property=Type", "--value"]).await else {
        return LoginSession::Unknown;
    };
    LoginSession::of_type(&kind)
}

impl LoginSession {
    /// What a display session of `kind` (its logind `Type`) says: `x11`, `wayland` and `mir` are
    /// graphical, anything else is a text login.
    #[must_use]
    pub fn of_type(kind: &str) -> Self {
        if matches!(kind, "x11" | "wayland" | "mir") {
            Self::Graphical
        } else {
            Self::NotGraphical
        }
    }
}
