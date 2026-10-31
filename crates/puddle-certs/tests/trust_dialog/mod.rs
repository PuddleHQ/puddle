// SPDX-License-Identifier: GPL-3.0-or-later
//! Adds the development certificate to `CurrentUser\Root` on a real Windows desktop and answers
//! the confirmation Windows shows for it, for the tests that need the add to end (`windows_store`
//! and `windows_browsers`).
//!
//! The add is a call that waits for a click, so it runs on its own thread while
//! `ci/trust-dialog.ps1` (UI Automation, started beside it) reads the dialog and presses the
//! button the test names. A dialog that never appears fails the test with the script's own
//! message; it never lets the add pass unanswered.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns: a failed setup fails the test; each test binary uses part of this"
)]

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use puddle_certs::devcert::{DevCertManager, DevCertReport, WindowsTrust};
use puddle_certs::{Location, Physical, StoreName, StoreSource, read_host_stores};

/// How long the answerer waits for the dialog to show up after the add starts.
pub(crate) const DIALOG_WAIT: Duration = Duration::from_secs(40);

/// The whole add, dialog and answer together. Under nextest's 120 s per test, with room for the
/// test's own work around it.
const OVERALL: Duration = Duration::from_secs(80);

const CU_ROOT: StoreSource =
    StoreSource::new(Location::CurrentUser, StoreName::Root, Physical::Default);

/// The button the answerer presses.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Answer {
    Yes,
    No,
}

impl Answer {
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Yes => "Yes",
            Self::No => "No",
        }
    }
}

/// What one answered add left behind.
pub(crate) struct Answered {
    pub(crate) report: DevCertReport,
    /// The dialog's texts and buttons, as the script printed them.
    pub(crate) dialog: String,
}

pub(crate) fn answerer_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ci/trust-dialog.ps1")
}

/// Runs the answerer against `owner_pid` and returns `Ok(its output)` on exit 0, `Err(its
/// output)` on any other exit (1: no dialog within `seconds`; 2: no such button).
pub(crate) fn run_answerer(
    action: &str,
    owner_pid: u32,
    seconds: u64,
    title: Option<&str>,
) -> Result<String, String> {
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(answerer_script())
        .args(["-Action", action, "-OwnerPid"])
        .arg(owner_pid.to_string())
        .args(["-Seconds", &seconds.to_string()]);
    if let Some(title) = title {
        command.args(["-Title", title]);
    }
    let out = command
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("starting the dialog answerer: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!(
            "the dialog answerer exited with {}: {text}",
            out.status
        ))
    }
}

enum Event {
    Answered(Result<String, String>),
    Ensured(Result<DevCertReport, String>),
}

/// Runs `manager.ensure` (which adds the certificate to `CurrentUser\Root` and so opens the
/// dialog) while the answerer presses `answer`. `Err` names what did not happen: no dialog, a
/// missing button, or an add that never returned.
pub(crate) fn ensure_answered(
    manager: &Arc<DevCertManager<WindowsTrust>>,
    answer: Answer,
) -> Result<Answered, String> {
    let (tx, rx) = mpsc::channel();
    let pid = std::process::id();
    let answering = tx.clone();
    std::thread::spawn(move || {
        let _ = answering.send(Event::Answered(run_answerer(
            answer.word(),
            pid,
            DIALOG_WAIT.as_secs(),
            None,
        )));
    });
    let manager = Arc::clone(manager);
    std::thread::spawn(move || {
        let ensured = manager
            .ensure(SystemTime::now())
            .map_err(|e| format!("ensure failed: {e}"));
        let _ = tx.send(Event::Ensured(ensured));
    });
    let start = Instant::now();
    let (mut dialog, mut report) = (None, None);
    while dialog.is_none() || report.is_none() {
        let left = OVERALL.saturating_sub(start.elapsed());
        match rx.recv_timeout(left) {
            Ok(Event::Answered(Ok(text))) => dialog = Some(text),
            Ok(Event::Answered(Err(e))) => {
                return Err(format!(
                    "the dialog was not answered {}: {e}",
                    answer.word()
                ));
            }
            Ok(Event::Ensured(r)) => report = Some(r?),
            Err(_) => {
                return Err(format!(
                    "no end after {OVERALL:?}: dialog answered: {}, add returned: {}",
                    dialog.is_some(),
                    report.is_some()
                ));
            }
        }
    }
    Ok(Answered {
        report: report.unwrap(),
        dialog: dialog.unwrap(),
    })
}

/// The SHA-1 thumbprint Windows shows for `der`, upper-case hex.
pub(crate) fn thumbprint(der: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("leaf.cer");
    std::fs::write(&file, der).unwrap();
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!(
            "(New-Object Security.Cryptography.X509Certificates.X509Certificate2('{}')).Thumbprint",
            file.display()
        ))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "reading the thumbprint: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// The dialog shows the thumbprint in groups of eight characters; the answer to "which
/// certificate am I about to trust" is only worth anything when it is the right one.
pub(crate) fn assert_dialog_names(dialog: &str, thumbprint: &str) {
    let squeezed: String = dialog
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    assert!(
        squeezed.contains(&thumbprint.to_ascii_uppercase()),
        "the dialog does not name thumbprint {thumbprint}: {dialog}"
    );
    assert!(
        dialog.contains("Security Warning"),
        "unexpected dialog title: {dialog}"
    );
}

/// The DER of every certificate in `CurrentUser\Root`, sorted.
pub(crate) fn current_user_root() -> Vec<Vec<u8>> {
    let mut all: Vec<Vec<u8>> = read_host_stores()
        .unwrap()
        .certs()
        .iter()
        .filter(|c| c.source == CU_ROOT)
        .map(|c| c.der.clone())
        .collect();
    all.sort();
    all
}

/// Removes the certificate from `CurrentUser\Root` when it goes out of scope, pass or fail.
/// The registry key is the store's own record of the certificate; deleting it needs no
/// confirmation (removing through the certificate API opens a second dialog).
pub(crate) struct RootGuard {
    thumbprint: String,
}

impl RootGuard {
    pub(crate) fn new(thumbprint: &str) -> Self {
        Self {
            thumbprint: thumbprint.to_owned(),
        }
    }

    pub(crate) fn remove(&self) {
        let key = format!(
            "HKCU\\Software\\Microsoft\\SystemCertificates\\Root\\Certificates\\{}",
            self.thumbprint
        );
        // Exit 1 means the key is not there, which is what the test wants.
        let _ = Command::new("reg.exe")
            .args(["delete", &key, "/f"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        self.remove();
    }
}
