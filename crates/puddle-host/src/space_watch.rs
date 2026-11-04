// SPDX-License-Identifier: GPL-3.0-or-later
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
pub(crate) trait Space: Send + Sync + 'static {
    /// Bytes free on the drive that holds the data folder.
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

/// What the watch remembers between reads: the level it last said.
#[derive(Debug)]
pub(crate) struct Watch {
    said: DiskSpaceLevel,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            said: DiskSpaceLevel::Ok,
        }
    }
}

impl Watch {
    /// The event for a read that found `level` with `free_bytes` free, or `None` when the level
    /// is the one already said.
    pub(crate) fn observe(&mut self, level: DiskSpaceLevel, free_bytes: u64) -> Option<Event> {
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

/// One read: measures the drive, classifies it by the lines the settings hold now and says what
/// changed. A drive the OS will not measure says nothing (the system check shows why).
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
            tracing::debug!(?failed, "the free space could not be read");
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
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use puddle_types::CollectingSink;
    use serde_json::json;

    use super::*;
    use crate::FileSettings;

    const GB: u64 = 1024 * 1024 * 1024;

    /// A drive whose free space the test sets; `u64::MAX` reads as an error.
    #[derive(Default)]
    struct Fake(AtomicU64);

    impl Fake {
        fn set(&self, bytes: u64) {
            self.0.store(bytes, Ordering::SeqCst);
        }
    }

    impl Space for Arc<Fake> {
        fn free_bytes(&self) -> io::Result<u64> {
            match self.0.load(Ordering::SeqCst) {
                u64::MAX => Err(io::Error::other("no such drive")),
                bytes => Ok(bytes),
            }
        }
    }

    struct Rig {
        drive: Arc<Fake>,
        settings: FileSettings,
        events: CollectingSink,
        watch: Watch,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        fn new(free: u64) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let drive = Arc::new(Fake::default());
            drive.set(free);
            Self {
                settings: FileSettings::new(dir.path()),
                drive,
                events: CollectingSink::default(),
                watch: Watch::default(),
                _dir: dir,
            }
        }

        /// Reads with the drive at `free` and returns what was said.
        async fn read(&mut self, free: u64) -> Vec<Event> {
            self.drive.set(free);
            read_once(
                &Arc::new(self.drive.clone()),
                &self.settings,
                &self.events,
                &mut self.watch,
            )
            .await;
            self.events.take()
        }
    }

    fn changed(level: DiskSpaceLevel, free_mib: u64) -> Event {
        Event::DiskSpaceChanged { level, free_mib }
    }

    #[tokio::test]
    async fn a_drive_that_is_fine_says_nothing_however_often_it_is_read() {
        let mut rig = Rig::new(100 * GB);
        for _ in 0..3 {
            assert_eq!(rig.read(100 * GB).await, []);
        }
    }

    #[tokio::test]
    async fn each_line_is_announced_once_when_crossed() {
        let mut rig = Rig::new(100 * GB);
        assert_eq!(
            rig.read(19 * GB).await,
            [changed(DiskSpaceLevel::Low, 19 * 1024)]
        );
        assert_eq!(rig.read(18 * GB).await, [], "still under the same line");
        assert_eq!(
            rig.read(4 * GB).await,
            [changed(DiskSpaceLevel::Critical, 4 * 1024)]
        );
        assert_eq!(rig.read(3 * GB).await, []);
    }

    #[tokio::test]
    async fn space_that_comes_back_is_said_and_a_new_crossing_is_announced_again() {
        let mut rig = Rig::new(100 * GB);
        rig.read(10 * GB).await;
        assert_eq!(
            rig.read(30 * GB).await,
            [changed(DiskSpaceLevel::Ok, 30 * 1024)]
        );
        assert_eq!(
            rig.read(10 * GB).await,
            [changed(DiskSpaceLevel::Low, 10 * 1024)]
        );
        // Back over the failure line but still under the warning line: the level moved, so it is said.
        rig.read(2 * GB).await;
        assert_eq!(
            rig.read(8 * GB).await,
            [changed(DiskSpaceLevel::Low, 8 * 1024)]
        );
    }

    #[tokio::test]
    async fn a_drive_already_low_at_the_first_read_is_a_crossing() {
        let mut rig = Rig::new(2 * GB);
        assert_eq!(
            rig.read(2 * GB).await,
            [changed(DiskSpaceLevel::Critical, 2 * 1024)]
        );
    }

    #[tokio::test]
    async fn the_lines_are_the_settings_read_at_every_round() {
        use puddle_api::SettingsRepo as _;
        let mut rig = Rig::new(100 * GB);
        assert_eq!(rig.read(30 * GB).await, []);
        rig.settings
            .save_global(json!({"ui": {"disk_warn_free_gb": 40, "disk_fail_free_gb": 10}}))
            .unwrap();
        assert_eq!(
            rig.read(30 * GB).await,
            [changed(DiskSpaceLevel::Low, 30 * 1024)]
        );
        assert_eq!(
            rig.read(9 * GB).await,
            [changed(DiskSpaceLevel::Critical, 9 * 1024)]
        );
    }

    #[tokio::test]
    async fn a_drive_that_cannot_be_read_says_nothing_and_keeps_what_was_said() {
        let mut rig = Rig::new(100 * GB);
        rig.read(10 * GB).await;
        assert_eq!(rig.read(u64::MAX).await, []);
        assert_eq!(
            rig.read(10 * GB).await,
            [],
            "still low: not announced twice"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_task_reads_after_the_first_wait_and_then_every_period() {
        let rig = Rig::new(2 * GB);
        let timing = FreeSpaceTiming {
            first_read: Duration::from_secs(10),
            period: Duration::from_secs(60),
        };
        let settings: Arc<dyn SettingsRepo> = Arc::new(rig.settings);
        let events = Arc::new(CollectingSink::default());
        let task = spawn(rig.drive.clone(), timing, settings, events.clone());
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(
            events.take(),
            [],
            "a window opening with Puddle needs the first wait"
        );
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert_eq!(events.take(), [changed(DiskSpaceLevel::Critical, 2 * 1024)]);
        rig.drive.set(50 * GB);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(events.take(), [], "not before the period is over");
        tokio::time::sleep(Duration::from_secs(40)).await;
        assert_eq!(events.take(), [changed(DiskSpaceLevel::Ok, 50 * 1024)]);
        task.abort();
    }

    #[test]
    fn the_real_drive_reads_a_folder_that_does_not_exist_yet() {
        let dir = tempfile::tempdir().unwrap();
        let drive = DriveOf(dir.path().join("later"));
        assert!(drive.free_bytes().unwrap() > 0);
    }
}
