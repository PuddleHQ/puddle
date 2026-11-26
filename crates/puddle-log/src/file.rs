// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log file: what the log bundle's recent logs are made of.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tracing_subscriber::fmt::MakeWriter;

/// The log file's name; the previous one keeps it with `.1` after.
pub const FILE_NAME: &str = "puddle.log";

/// The size at which the log file is moved aside, so the two files hold at most twice this.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// A log file that rolls over at a size: `puddle.log` is renamed to `puddle.log.1` (replacing the
/// one before) and a new one starts. It starts without a file, because a program sets up logging
/// before it knows its data folder; lines logged until [`RollingFile::attach`] only reach stderr.
#[derive(Debug, Default)]
pub struct RollingFile {
    open: OnceLock<Open>,
}

#[derive(Debug)]
struct Open {
    path: PathBuf,
    max: u64,
    state: Mutex<(File, u64)>,
}

impl RollingFile {
    /// Starts writing to `folder`'s log file, appending to what is there, making the folder. The
    /// first call wins.
    ///
    /// # Errors
    ///
    /// The I/O error of making the folder or opening the file.
    pub fn attach(&self, folder: &Path, max: u64) -> io::Result<()> {
        fs::create_dir_all(folder)?;
        let path = folder.join(FILE_NAME);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        let _ = self.open.set(Open {
            path,
            max,
            state: Mutex::new((file, size)),
        });
        Ok(())
    }

    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        match self.open.get() {
            Some(open) => open.write_all(buf),
            None => Ok(()),
        }
    }
}

impl Open {
    fn write_all(&self, buf: &[u8]) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (file, size) = &mut *state;
        if *size >= self.max {
            let old = self.path.with_extension("log.1");
            // A failed move leaves the file growing, which beats losing the log.
            if fs::rename(&self.path, &old).is_ok() {
                *file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?;
                *size = 0;
            }
        }
        file.write_all(buf)?;
        *size += buf.len() as u64;
        Ok(())
    }
}

/// One write to a [`RollingFile`].
#[derive(Debug)]
pub(crate) struct RollingWriter<'a>(&'a RollingFile);

#[cfg(test)]
impl<'a> RollingWriter<'a> {
    pub(crate) fn of(file: &'a RollingFile) -> Self {
        Self(file)
    }
}

impl Write for RollingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A [`RollingFile`] the log layer and [`crate::LogControl`] both hold.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedFile(pub(crate) Arc<RollingFile>);

impl SharedFile {
    pub(crate) fn file(&self) -> &RollingFile {
        &self.0
    }
}

impl<'a> MakeWriter<'a> for SharedFile {
    type Writer = RollingWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        RollingWriter(&self.0)
    }
}
