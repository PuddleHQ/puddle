// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crash file: a panic anywhere in the shell writes what happened to a file in the data
//! folder. Nothing is sent anywhere; the file is there for the user to attach to a bug report if
//! they choose to.

use std::backtrace::Backtrace;
use std::fs::{self, File};
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The folder of crash files inside the data folder.
pub const FOLDER: &str = "crashes";

/// What every crash file's name ends with.
const CRASH_EXTENSION: &str = ".txt";

/// How many crash files are kept; the oldest go first, so a crash loop cannot fill the disk.
pub const KEEP: usize = 10;

/// What a crash file says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The program's version.
    pub version: String,
    /// Operating system and architecture, as `std::env::consts` names them.
    pub platform: String,
    /// The thread that panicked, by name (`<unnamed>` when it has none).
    pub thread: String,
    /// Where in the source it panicked.
    pub location: String,
    /// The panic message.
    pub message: String,
    /// The backtrace, as text.
    pub backtrace: String,
}

impl Report {
    /// The report of `info`, with the backtrace as it is now.
    #[must_use]
    pub fn of(info: &PanicHookInfo<'_>) -> Self {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_owned());
        Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
            thread: std::thread::current()
                .name()
                .unwrap_or("<unnamed>")
                .to_owned(),
            location: info
                .location()
                .map_or_else(|| "(unknown)".to_owned(), ToString::to_string),
            message,
            backtrace: Backtrace::force_capture().to_string(),
        }
    }

    /// The file's text.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "Puddle crashed. This file stays on this computer; nothing was sent anywhere.\n\
             Attach it to a bug report if you want to.\n\n\
             version:  {}\nplatform: {}\nthread:   {}\nat:       {}\nmessage:  {}\n\n\
             backtrace:\n{}\n",
            self.version, self.platform, self.thread, self.location, self.message, self.backtrace,
        )
    }
}

/// Writes `report` as a new file in `dir` (created if needed) named after `now` and this
/// process, then deletes the oldest files beyond [`KEEP`]. Returns the new file's path.
///
/// # Errors
///
/// Whatever the file system reports; a failed prune is not an error.
pub fn write(dir: &Path, report: &Report, now: SystemTime) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let millis = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let path = dir.join(format!(
        "crash-{millis:017}-{}{CRASH_EXTENSION}",
        std::process::id()
    ));
    let mut file = File::create_new(&path)?;
    file.write_all(report.render().as_bytes())?;
    file.sync_all()?;
    prune(dir, KEEP);
    Ok(path)
}

/// Deletes the oldest `crash-*.txt` files in `dir` until `keep` remain. Names sort by time.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-") && n.ends_with(CRASH_EXTENSION))
        })
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for old in files.into_iter().take(excess) {
        // A file that cannot be deleted stays; the next crash tries again.
        let _ = fs::remove_file(old);
    }
}

/// Makes every panic write a crash file into `dir`, then run the hook that was there before (the
/// message on stderr).
pub fn install(dir: PathBuf) {
    puddle_types::panic_hook::chain(move |info| {
        match write(&dir, &Report::of(info), SystemTime::now()) {
            Ok(path) => {
                tracing::error!(file = %path.display(), "Puddle crashed; wrote a crash file");
            }
            Err(err) => tracing::error!(%err, "Puddle crashed and could not write a crash file"),
        }
    });
}

#[cfg(test)]
mod tests;
