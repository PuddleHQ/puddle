// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(windows)]
pub(in super::super) const IDLE: (&str, &[&str]) = ("ping", &["-n", "30", "127.0.0.1"]);
#[cfg(not(windows))]
pub(in super::super) const IDLE: (&str, &[&str]) = ("sleep", &["30"]);

#[cfg(windows)]
pub(in super::super) const BUSY: (&str, &[&str]) = ("cmd", &["/c", "for /l %i in (0,0,1) do @rem"]);
#[cfg(not(windows))]
pub(in super::super) const BUSY: (&str, &[&str]) = ("sh", &["-c", "while :; do :; done"]);

#[cfg(windows)]
pub(in super::super) const SHELL_WITH_A_CHILD: (&str, &[&str], &str) = (
    "cmd",
    &["/c", "ping -n 30 127.0.0.1 >nul & exit 0"],
    "PING.EXE",
);
#[cfg(not(windows))]
pub(in super::super) const SHELL_WITH_A_CHILD: (&str, &[&str], &str) =
    ("sh", &["-c", "sleep 30; true"], "sleep");
