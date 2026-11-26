// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log bundle: what a user attaches to "upload a debug log".
//!
//! One `.tar.gz` with the versions and platform, the settings with their secrets cut out and the
//! recent log files. Every text passes through [`crate::redact`] on its way in, whatever wrote it.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::Value;

use crate::file::FILE_NAME;
use crate::redact::{redact, redact_json};

/// What goes into a bundle besides the log files.
#[derive(Debug, Clone)]
pub struct Bundle<'a> {
    /// `name version` lines, one per program or component.
    pub versions: &'a [(String, String)],
    /// The operating system and architecture, as the caller names them.
    pub platform: &'a str,
    /// The level the log is at now.
    pub log_level: &'a str,
    /// The settings document, if one could be read.
    pub settings: Option<Value>,
    /// The folder the log files are in.
    pub log_folder: &'a Path,
    /// The user's home folder, replaced by `~` in every text.
    pub home: Option<&'a str>,
    /// When the bundle was made, in seconds since 1970.
    pub generated: u64,
}

/// A bundle that could not be written.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// Reading a log file or writing the archive failed.
    #[error("writing the log bundle failed: {0}")]
    Io(#[from] io::Error),
    /// The settings could not be written as JSON.
    #[error("writing the settings into the log bundle failed: {0}")]
    Json(#[from] serde_json::Error),
}

fn clean(text: &str, home: Option<&str>) -> String {
    let text = redact(text);
    match home.filter(|h| h.len() > 1) {
        Some(home) => text.replace(home, "~"),
        None => text,
    }
}

fn add<W: Write>(
    archive: &mut tar::Builder<W>,
    name: &str,
    text: &str,
    when: u64,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(text.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(when);
    archive.append_data(&mut header, name, text.as_bytes())
}

/// Writes the bundle to `out` as a gzipped tar.
///
/// # Errors
///
/// [`BundleError`] when a log file exists and cannot be read, or `out` cannot be written. A missing
/// log folder is no error: the bundle then has no log, which the info file says.
pub fn write_bundle(bundle: &Bundle<'_>, out: impl Write) -> Result<(), BundleError> {
    let mut archive = tar::Builder::new(GzEncoder::new(out, Compression::default()));
    let home = bundle.home;

    let mut info = String::new();
    for (name, version) in bundle.versions {
        let _ = writeln!(info, "{name} {version}");
    }
    let _ = writeln!(info, "platform {}", bundle.platform);
    let _ = writeln!(info, "log level {}", bundle.log_level);
    let _ = writeln!(info, "made at {} (seconds since 1970)", bundle.generated);
    let names = [format!("{FILE_NAME}.1"), FILE_NAME.to_owned()];
    let mut found = 0;
    let mut logs = Vec::new();
    for name in &names {
        match fs::read(bundle.log_folder.join(name)) {
            Ok(bytes) => {
                found += 1;
                logs.push((name, String::from_utf8_lossy(&bytes).into_owned()));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    if found == 0 {
        info.push_str("no log files: logging to a file starts when Puddle does\n");
    }
    add(
        &mut archive,
        "info.txt",
        &clean(&info, home),
        bundle.generated,
    )?;

    if let Some(mut settings) = bundle.settings.clone() {
        redact_json(&mut settings);
        let text = serde_json::to_string_pretty(&settings)?;
        add(
            &mut archive,
            "settings.json",
            &clean(&text, home),
            bundle.generated,
        )?;
    }
    for (name, text) in logs {
        add(
            &mut archive,
            &format!("logs/{name}"),
            &clean(&text, home),
            bundle.generated,
        )?;
    }
    archive.into_inner()?.finish()?;
    Ok(())
}
