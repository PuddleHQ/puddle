// SPDX-License-Identifier: AGPL-3.0-or-later
//! Watches the free space on the drive that holds Puddle's data folder while nobody asks.
//!
//! The system check reads the space when it runs; a drive that fills up during a long build in a
//! workspace changes none of that, so the host reads it now and then and says
//! ([`Event::DiskSpaceChanged`]) when it crosses the warning or the failure line, or recovers.
//! One event per crossing: staying under a line says nothing more, and a drive that was fine
//! when Puddle started and is low at the first read counts as a crossing.

use puddle_api::SettingsRepo;
use puddle_types::{DiskSpaceLevel, Event, EventSink};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::FreeSpaceTiming;
use crate::doctor::disk_limits;

/// Where the free space comes from: the real drive, or a test's fake.
pub trait Space: Send + Sync + 'static {
    /// Bytes free on the drive that holds the data folder.
    ///
    /// # Errors
    ///
    /// The drive's figure cannot be read.
    fn free_bytes(&self) -> io::Result<u64>;
}

/// The drive that holds one folder.
#[derive(Debug, Clone)]
pub(crate) struct DriveOf(pub(crate) PathBuf);

impl Space for DriveOf {
    fn free_bytes(&self) -> io::Result<u64> {
        puddle_fs::free_bytes(&self.0)
    }
}

/// The given space, or the real drive that holds `data`.
pub(crate) fn space_or_drive(
    given: Option<Arc<dyn Space>>,
    data: &std::path::Path,
) -> Arc<dyn Space> {
    given.unwrap_or_else(|| Arc::new(DriveOf(data.to_path_buf())))
}

/// What the watch remembers between reads: the level it last said.
#[derive(Debug)]
pub(crate) struct Watch {
    said: DiskSpaceLevel,
    /// The last read failed, and that was already logged.
    unreadable: bool,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            said: DiskSpaceLevel::Ok,
            unreadable: false,
        }
    }
}

impl Watch {
    /// The event for a read that found `level` with `free_bytes` free, or `None` when the level
    /// is the one already said.
    pub(crate) fn observe(&mut self, level: DiskSpaceLevel, free_bytes: u64) -> Option<Event> {
        self.unreadable = false;
        if level == self.said {
            return None;
        }
        self.said = level;
        Some(Event::DiskSpaceChanged {
            level,
            free_mib: free_bytes / (1024 * 1024),
        })
    }
}

impl Watch {
    /// Notes a failed read; true for the first one of a run of failures, so the log gets one
    /// line per episode and not one per minute.
    fn note_unreadable(&mut self) -> bool {
        !std::mem::replace(&mut self.unreadable, true)
    }
}

/// One read: measures the drive, classifies it by the lines the settings hold now and says what
/// changed. A drive the OS will not measure says nothing to the app (the system check shows why)
/// and logs one warning per episode, until a read works again.
pub(crate) async fn read_once<S: Space>(
    space: &Arc<S>,
    settings: &dyn SettingsRepo,
    events: &dyn EventSink,
    watch: &mut Watch,
) {
    let reading = Arc::clone(space);
    let free = match tokio::task::spawn_blocking(move || reading.free_bytes()).await {
        Ok(Ok(free)) => free,
        failed => {
            if watch.note_unreadable() {
                tracing::warn!(?failed, "the free space could not be read");
            }
            return;
        }
    };
    let level = disk_limits(settings).level(free);
    if let Some(event) = watch.observe(level, free) {
        tracing::info!(?level, free, "free disk space changed level");
        events.emit(event);
    }
}

/// Reads after `timing.first_read` and then every `timing.period` until the returned task is
/// aborted. A drive takes minutes to fill and a read is one system call, so the period is a minute.
pub(crate) fn spawn<S: Space>(
    space: S,
    timing: FreeSpaceTiming,
    settings: Arc<dyn SettingsRepo>,
    events: Arc<dyn EventSink>,
) -> tokio::task::JoinHandle<()> {
    let space = Arc::new(space);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + timing.first_read,
            timing.period,
        );
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut watch = Watch::default();
        loop {
            tick.tick().await;
            read_once(&space, settings.as_ref(), events.as_ref(), &mut watch).await;
        }
    })
}

#[cfg(test)]
mod tests;
