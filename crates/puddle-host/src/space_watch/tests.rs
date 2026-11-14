// SPDX-License-Identifier: AGPL-3.0-or-later
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
    // Under a capture: a read that fails outside one would cache the log call site as off
    // for the capture test running beside it.
    let mut said = vec![];
    logged(async { said = rig.read(u64::MAX).await }).await;
    assert_eq!(said, []);
    assert_eq!(
        rig.read(10 * GB).await,
        [],
        "still low: not announced twice"
    );
}

/// The log lines written while `run` polled on this thread.
async fn logged<F: Future<Output = ()>>(run: F) -> String {
    use std::sync::Mutex;
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Another test may already have run the call site with no subscriber and cached "off".
    tracing::callsite::rebuild_interest_cache();
    run.await;
    io::Write::flush(&mut capture.clone()).unwrap();
    let bytes = capture.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn an_unreadable_drive_logs_one_warning_per_episode_and_a_good_read_ends_the_episode() {
    let mut rig = Rig::new(100 * GB);
    let first = logged(async {
        rig.read(u64::MAX).await;
    })
    .await;
    assert_eq!(first.matches("WARN").count(), 1, "{first}");
    assert!(first.contains("could not be read"), "{first}");
    let again = logged(async {
        rig.read(u64::MAX).await;
    })
    .await;
    assert_eq!(again, "", "same episode: quiet");
    let good = logged(async {
        rig.read(100 * GB).await;
    })
    .await;
    assert_eq!(good, "");
    let next = logged(async {
        rig.read(u64::MAX).await;
    })
    .await;
    assert_eq!(
        next.matches("WARN").count(),
        1,
        "a new episode warns again: {next}"
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
fn no_given_space_means_the_real_drive_and_a_given_one_wins() {
    let dir = tempfile::tempdir().unwrap();
    assert!(space_or_drive(None, dir.path()).free_bytes().unwrap() > 0);
    let fake = Arc::new(Fake::default());
    fake.set(7);
    let fixed: Arc<dyn Space> = Arc::new(fake);
    assert_eq!(
        space_or_drive(Some(fixed), dir.path())
            .free_bytes()
            .unwrap(),
        7
    );
}

#[test]
fn the_real_drive_reads_a_folder_that_does_not_exist_yet() {
    let dir = tempfile::tempdir().unwrap();
    let drive = DriveOf(dir.path().join("later"));
    assert!(drive.free_bytes().unwrap() > 0);
}
