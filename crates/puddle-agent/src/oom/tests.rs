// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use proptest::prelude::*;

use super::*;

#[expect(
    clippy::unnecessary_wraps,
    reason = "compared with parse results, which are Options"
)]
fn kill(pid: u32, process: &str) -> Option<Kill> {
    Some(Kill {
        pid,
        process: process.into(),
    })
}

#[test]
fn global_and_memcg_kill_lines_are_parsed() {
    assert_eq!(
        parse_kmsg_record(
            "3,1,2,-;Out of memory: Killed process 2406 (tail) total-vm:7280204kB, anon-rss:7274112kB, file-rss:0kB\n"
        ),
        kill(2406, "tail")
    );
    assert_eq!(
        parse_kmsg_record(
            "3,99,123456,-,caller=T42;Memory cgroup out of memory: Killed process 77 (node) total-vm:1kB\n SUBSYSTEM=memory\n"
        ),
        kill(77, "node")
    );
    // Older kernels: no total-vm field after the name.
    assert_eq!(
        parse_kmsg_record("0,5,6,-;Killed process 9 (a b)\n"),
        kill(9, "a b")
    );
}

#[test]
fn a_name_with_parentheses_and_spaces_is_kept_whole() {
    assert_eq!(
        parse_kmsg_record("3,1,2,-;Out of memory: Killed process 5 (x) (y) total-vm:1kB\n"),
        kill(5, "x) (y")
    );
}

#[test]
fn records_that_are_not_kernel_kill_lines_are_ignored() {
    for record in [
        // Userspace writes to /dev/kmsg never get facility 0 (here user.err = 11).
        "11,1,2,-;Out of memory: Killed process 1 (fake) total-vm:1kB\n",
        "3,1,2,-;oom_reaper: reaped process 2406 (tail), now anon-rss:0kB\n",
        "3,1,2,-;Killed process x (tail) total-vm:1kB\n",
        "3,1,2,-;Killed process 1 tail\n",
        "3,1,2,-;Killed process 1 (tail\n",
        "3,1,2,-;Killed process 1\n",
        "no header here Killed process 1 (a)\n",
        "x,1,2,-;Killed process 1 (a)\n",
        "6,2,3,-;eth0: link up\n",
        " SUBSYSTEM=memory\n",
        "",
    ] {
        assert_eq!(parse_kmsg_record(record), None, "{record:?}");
    }
    // A kill line only in a continuation line doesn't count either.
    assert_eq!(
        parse_kmsg_record("6,1,2,-;hello\n Killed process 1 (a)\n"),
        None
    );
}

#[test]
fn the_vmstat_counter_is_found_or_missing() {
    assert_eq!(
        parse_vmstat("nr_free_pages 1\noom_kill 12\nx 3\n"),
        Some(12)
    );
    assert_eq!(parse_vmstat("nr_free_pages 1\n"), None);
    assert_eq!(parse_vmstat("oom_kill lots\n"), None);
    assert_eq!(parse_vmstat("oom_kill_other 3\n"), None);
}

proptest! {
    #[test]
    fn any_record_parses_or_is_ignored_without_panicking(s in "\\PC{0,200}") {
        let _ = parse_kmsg_record(&s);
        let _ = parse_vmstat(&s);
    }

    #[test]
    fn a_formatted_kill_line_round_trips(pid in any::<u32>(), name in "[a-zA-Z0-9_./ ()-]{1,15}") {
        let record = format!("3,7,8,-;Out of memory: Killed process {pid} ({name}) total-vm:10kB, anon-rss:1kB\n");
        prop_assert_eq!(parse_kmsg_record(&record), kill(pid, &name));
    }
}

const GRACE: Duration = Duration::from_secs(1);

#[test]
fn a_kill_logged_first_then_counted_is_reported_once() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    assert_eq!(c.on_logged(t0), Logged::New);
    c.on_counted(1, t0);
    assert_eq!(c.due(t0 + Duration::from_secs(10)), 0);
}

#[test]
fn a_kill_counted_first_then_logged_is_reported_once() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    c.on_counted(1, t0);
    assert_eq!(c.due(t0), 0);
    assert_eq!(c.on_logged(t0), Logged::New);
    assert_eq!(c.due(t0 + Duration::from_secs(10)), 0);
}

#[test]
fn a_counted_kill_without_a_log_line_is_due_after_the_grace_period() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    c.on_counted(3, t0);
    assert_eq!(c.on_logged(t0), Logged::New);
    assert_eq!(c.due(t0 + Duration::from_millis(999)), 0);
    c.on_counted(1, t0 + Duration::from_millis(500));
    assert_eq!(c.due(t0 + Duration::from_secs(1)), 2);
    assert_eq!(c.due(t0 + Duration::from_millis(1500)), 1);
    assert_eq!(c.due(t0 + Duration::from_secs(9)), 0);
}

#[test]
fn log_lines_ahead_of_the_counter_claim_later_increases() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    assert_eq!(c.on_logged(t0), Logged::New);
    assert_eq!(c.on_logged(t0), Logged::New);
    c.on_counted(3, t0);
    assert_eq!(c.due(t0 + GRACE), 1);
}

#[test]
fn a_counted_kill_claimed_by_a_line_leaves_nothing_for_the_next_line_to_claim() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    assert_eq!(c.on_logged(t0), Logged::New);
    c.on_counted(1, t0);
    // This line is a second kill, ahead of its counter, so the second increase is claimed too.
    assert_eq!(c.on_logged(t0), Logged::New);
    c.on_counted(1, t0);
    assert_eq!(c.due(t0 + GRACE), 0);
}

#[test]
fn a_log_line_after_its_unnamed_report_is_the_same_kill() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    c.on_counted(1, t0);
    assert_eq!(c.due(t0 + GRACE), 1);
    assert_eq!(c.on_logged(t0 + GRACE), Logged::AlreadyReported);
    // The line left no stray credit behind: the next counted kill is still reported.
    c.on_counted(1, t0 + GRACE);
    assert_eq!(c.due(t0 + GRACE * 2), 1);
}

#[test]
fn late_lines_cancel_one_unnamed_report_each() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    c.on_counted(2, t0);
    assert_eq!(c.due(t0 + GRACE), 2);
    assert_eq!(c.on_logged(t0 + GRACE), Logged::AlreadyReported);
    assert_eq!(c.on_logged(t0 + GRACE), Logged::AlreadyReported);
    // Both reports are used up: a third line is a new kill, ahead of its counter.
    assert_eq!(c.on_logged(t0 + GRACE), Logged::New);
    c.on_counted(1, t0 + GRACE);
    assert_eq!(c.due(t0 + GRACE * 3), 0);
}

#[test]
fn a_late_line_cancels_the_oldest_unnamed_report() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    c.on_counted(1, t0);
    c.on_counted(1, t0 + Duration::from_millis(600));
    // Reported at t0 + 1.0 s and t0 + 1.6 s.
    assert_eq!(c.due(t0 + GRACE), 1);
    assert_eq!(c.due(t0 + Duration::from_millis(1600)), 1);
    assert_eq!(
        c.on_logged(t0 + Duration::from_millis(1600)),
        Logged::AlreadyReported
    );
    // The first report went; the second is still within the period at 2.2 s (the first isn't).
    assert_eq!(
        c.on_logged(t0 + Duration::from_millis(2200)),
        Logged::AlreadyReported
    );
    assert_eq!(c.on_logged(t0 + Duration::from_millis(2200)), Logged::New);
}

#[test]
fn an_unnamed_report_is_forgotten_after_the_grace_period() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    c.on_counted(1, t0);
    assert_eq!(c.due(t0 + GRACE), 1);
    // Just inside the period the line still cancels the report; at the period's end it doesn't.
    let mut inside = Correlator::new(GRACE);
    inside.on_counted(1, t0);
    assert_eq!(inside.due(t0 + GRACE), 1);
    assert_eq!(
        inside.on_logged(t0 + GRACE + GRACE - Duration::from_millis(1)),
        Logged::AlreadyReported
    );
    assert_eq!(c.on_logged(t0 + GRACE + GRACE), Logged::New);
}

#[test]
fn a_log_line_ahead_of_a_counter_that_never_moves_is_forgotten_after_the_grace_period() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    assert_eq!(c.on_logged(t0), Logged::New);
    // Counter moves later than the grace period: the line no longer covers it.
    c.on_counted(1, t0 + GRACE);
    assert_eq!(c.due(t0 + GRACE * 2), 1);

    // Just inside the period it still does.
    let mut inside = Correlator::new(GRACE);
    assert_eq!(inside.on_logged(t0), Logged::New);
    inside.on_counted(1, t0 + GRACE - Duration::from_millis(1));
    assert_eq!(inside.due(t0 + GRACE * 3), 0);
}

#[test]
fn a_stale_log_line_ahead_is_forgotten_when_the_next_line_comes() {
    let t0 = Instant::now();
    let mut c = Correlator::new(GRACE);
    assert_eq!(c.on_logged(t0), Logged::New);
    assert_eq!(c.on_logged(t0 + GRACE), Logged::New);
    // Only the second line is still ahead.
    c.on_counted(2, t0 + GRACE);
    assert_eq!(c.due(t0 + GRACE * 2), 1);
}

/// Collects what the tracing subscriber writes, so a test can count warnings.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        tracing_subscriber::fmt()
            .with_writer(self.clone())
            .with_ansi(false)
            .finish()
    }

    fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Write for Logs {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[tokio::test]
async fn a_counter_read_failure_is_warned_once_per_outage() {
    let logs = Logs::default();
    let _subscriber = tracing::subscriber::set_default(logs.subscriber());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vmstat");
    let warning = "oom_kill counter read failed";
    let mut failing = false;

    std::fs::write(&path, "oom_kill 1\n").unwrap();
    assert_eq!(read_counter(&path, &mut failing).await, Some(1));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(read_counter(&path, &mut failing).await, None);
    assert_eq!(read_counter(&path, &mut failing).await, None);
    assert_eq!(logs.text().matches(warning).count(), 1);

    std::fs::write(&path, "oom_kill 2\n").unwrap();
    assert_eq!(read_counter(&path, &mut failing).await, Some(2));
    std::fs::remove_file(&path).unwrap();
    assert_eq!(read_counter(&path, &mut failing).await, None);
    assert_eq!(logs.text().matches(warning).count(), 2);
}

/// A temp dir with a fake `vmstat` and `kmsg`, removed on drop.
struct Fake {
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

impl Fake {
    fn new(tag: &str, counter: u64) -> Self {
        let tmp = tempfile::Builder::new()
            .prefix(&format!("puddle-agent-oom-{tag}-"))
            .tempdir()
            .unwrap();
        let fake = Self {
            dir: tmp.path().to_path_buf(),
            _tmp: tmp,
        };
        fake.set_counter(counter);
        std::fs::write(fake.kmsg(), "6,1,1,-;old boot noise\n3,2,2,-;Out of memory: Killed process 1 (before) total-vm:1kB\n").unwrap();
        fake
    }
    fn vmstat(&self) -> PathBuf {
        self.dir.join("vmstat")
    }
    fn kmsg(&self) -> PathBuf {
        self.dir.join("kmsg")
    }
    fn set_counter(&self, n: u64) {
        std::fs::write(self.vmstat(), format!("pgfault 1\noom_kill {n}\n")).unwrap();
    }
    fn log(&self, record: &str) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.kmsg())
            .unwrap();
        f.write_all(record.as_bytes()).unwrap();
    }
    fn sources(&self) -> OomSources {
        OomSources {
            vmstat: self.vmstat(),
            kmsg: self.kmsg(),
            poll: Duration::from_millis(20),
            grace: Duration::from_millis(200),
        }
    }
}

/// What the watch has read so far: how many readings, and the counter value of the latest.
#[derive(Clone, Copy)]
struct Readings {
    taken: u64,
    counter: Option<u64>,
}

/// Follows the watch's counter readings: the first is its baseline.
struct Polls {
    readings: tokio::sync::watch::Receiver<Readings>,
    queued: std::sync::mpsc::Receiver<()>,
}

impl Polls {
    /// Starts [`watch_observed`] and returns its task with the readings.
    fn start(
        sources: OomSources,
        out: mpsc::Sender<AgentMessage>,
    ) -> (tokio::task::JoinHandle<()>, Self) {
        let (tx, rx) = tokio::sync::watch::channel(Readings {
            taken: 0,
            counter: None,
        });
        let (queued_tx, queued) = std::sync::mpsc::channel();
        let task = tokio::spawn(watch_observed(
            sources,
            out,
            move |counter| {
                tx.send_modify(|r| {
                    r.taken += 1;
                    r.counter = counter;
                });
            },
            move || {
                let _ = queued_tx.send(());
            },
        ));
        (
            task,
            Self {
                readings: rx,
                queued,
            },
        )
    }

    // flake-ok: waits on the watch channel the reader publishes to (no polling); bounded to 5 s, and the
    // panic names what it waited for.
    async fn until(&mut self, what: &str, mut done: impl FnMut(Readings) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), self.readings.wait_for(|&r| done(r)))
            .await
            .unwrap_or_else(|_| panic!("the watch {what}"))
            .expect("the watch is running");
    }

    /// Waits until the baseline reading is made.
    async fn baseline(&mut self) {
        self.until("reads its baseline", |r| r.taken >= 1).await;
    }

    /// Waits for two more readings: the second began after this call, so the loop has ticked at
    /// least once in full since. An absence window opens after this, not at the spawn.
    async fn ticks(&mut self) {
        let start = self.readings.borrow().taken;
        self.until("keeps polling", |r| r.taken >= start + 2).await;
    }

    /// Waits until a reading has seen the counter at `value`. Waiting for a reading by count would
    /// not do: one that began before the test changed the counter can end after it.
    async fn counter_at(&mut self, value: u64) {
        self.until("reads the changed counter", |r| r.counter == Some(value))
            .await;
    }

    /// Blocks this thread until the log reader has queued a kill. The reader is a thread of its
    /// own, and under load it can lag by more than a grace period.
    fn blocking_until_log_kill_queued(&self) {
        self.queued
            .recv_timeout(Duration::from_secs(10))
            .expect("the log reader queues the kill");
    }
}

async fn next(rx: &mut mpsc::Receiver<AgentMessage>, within: Duration) -> Option<AgentMessage> {
    tokio::time::timeout(within, rx.recv()).await.ok().flatten()
}

#[tokio::test]
async fn watch_reports_a_logged_kill_once_with_its_name_and_nothing_from_before_it_started() {
    let fake = Fake::new("named", 5);
    let (tx, mut rx) = mpsc::channel(8);
    let (task, mut polls) = Polls::start(fake.sources(), tx);
    // Nothing yet: the counter's baseline is 5 and the old log line is skipped.
    polls.baseline().await;
    polls.ticks().await;
    assert_eq!(next(&mut rx, Duration::from_millis(300)).await, None);

    fake.set_counter(6);
    fake.log("3,3,3,-;Out of memory: Killed process 4242 (hog) total-vm:9kB\n");
    assert_eq!(
        next(&mut rx, Duration::from_secs(2)).await,
        Some(AgentMessage::oom_kill(Some(4242), Some("hog".into())))
    );
    // The counter increase was claimed by the log line: no unnamed report follows. The window
    // starts once the counter's increase has been read and a grace period could have run out.
    polls.counter_at(6).await;
    polls.ticks().await;
    assert_eq!(next(&mut rx, Duration::from_millis(500)).await, None);
    drop(rx);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

/// The runtime stalls (a loaded host) after the counter moved and just as the log line arrives, so
/// when it runs again the line and the grace period's end are both ready. The line must win: the
/// kill is reported once, named. Before the watch loop served log kills first it was a coin toss per
/// kill, so ten kills in a row make a miss all but certain.
#[tokio::test]
async fn a_log_line_ready_together_with_the_end_of_the_grace_period_is_served_first() {
    let fake = Fake::new("stall", 0);
    let (tx, mut rx) = mpsc::channel(8);
    let grace = fake.sources().grace;
    let (task, mut polls) = Polls::start(fake.sources(), tx);
    polls.baseline().await;
    // flake-ok: repeats the whole scenario ten times and stalls the thread on purpose, nothing is polled
    for round in 1..=10_u32 {
        fake.set_counter(u64::from(round));
        // The grace period (200 ms) runs from the reading that sees the change. After that
        // reading nothing awaits before the stall below, so the line is in before the grace ends.
        polls.counter_at(u64::from(round)).await;
        let seen = std::time::Instant::now();
        fake.log(&format!(
            "3,{round},{round},-;Out of memory: Killed process {round} (hog) total-vm:9kB\n"
        ));
        // Block this runtime's only thread, the watch task with it, until the line is queued and
        // the grace period is over; the log reader has its own thread.
        polls.blocking_until_log_kill_queued();
        std::thread::sleep(grace.saturating_sub(seen.elapsed()));
        assert_eq!(
            next(&mut rx, Duration::from_secs(2)).await,
            Some(AgentMessage::oom_kill(Some(round), Some("hog".into()))),
            "round {round}"
        );
    }
    polls.ticks().await;
    assert_eq!(next(&mut rx, Duration::from_millis(400)).await, None);
    task.abort();
}

/// The log line arrives while the tick that finds the grace period over is reading the counter.
/// Nothing here depends on timing: the watch's own thread is held (a stalled counter read) until
/// the grace period is over and, on the next tick, until the line is queued.
#[tokio::test]
async fn a_log_line_arriving_during_the_counter_read_that_ends_the_grace_period_is_served_first() {
    let fake = Fake::new("midread", 0);
    let (tx, mut rx) = mpsc::channel(8);
    let sources = fake.sources();
    let grace = sources.grace;
    let kmsg = fake.kmsg();
    let (queued_tx, queued) = std::sync::mpsc::channel();
    let mut stage = 0_u8;
    let (baseline_tx, baseline) = tokio::sync::oneshot::channel();
    let mut baseline_tx = Some(baseline_tx);
    let task = tokio::spawn(watch_observed(
        sources,
        tx,
        move |counter| {
            if let Some(done) = baseline_tx.take() {
                let _ = done.send(());
            } else if counter == Some(1) && stage == 0 {
                // The reading that sees the kill: stall until its grace period is over, so the
                // next tick starts with the entry due.
                stage = 1;
                std::thread::sleep(grace + Duration::from_millis(50));
            } else if stage == 1 {
                // The next tick's read is done and its due check is next: the line lands now.
                stage = 2;
                let mut f = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&kmsg)
                    .unwrap();
                f.write_all(b"3,3,3,-;Out of memory: Killed process 9 (hog) total-vm:9kB\n")
                    .unwrap();
                queued
                    .recv_timeout(Duration::from_secs(10))
                    .expect("the log reader queues the kill");
            }
        },
        move || {
            let _ = queued_tx.send(());
        },
    ));
    // The counter must not move before the watch has read its baseline, or that reading is the
    // kill's own and nothing is counted.
    tokio::time::timeout(Duration::from_secs(5), baseline)
        .await
        .expect("the watch reads its baseline")
        .expect("the watch is running");
    fake.set_counter(1);
    assert_eq!(
        next(&mut rx, Duration::from_secs(5)).await,
        Some(AgentMessage::oom_kill(Some(9), Some("hog".into())))
    );
    assert_eq!(next(&mut rx, Duration::from_millis(400)).await, None);
    task.abort();
}

#[tokio::test]
async fn watch_reports_a_counted_kill_without_a_log_line_unnamed() {
    let fake = Fake::new("unnamed", 0);
    let (tx, mut rx) = mpsc::channel(8);
    let (task, mut polls) = Polls::start(fake.sources(), tx);
    polls.baseline().await;
    fake.set_counter(2);
    assert_eq!(
        next(&mut rx, Duration::from_secs(2)).await,
        Some(AgentMessage::oom_kill(None, None))
    );
    assert_eq!(
        next(&mut rx, Duration::from_secs(2)).await,
        Some(AgentMessage::oom_kill(None, None))
    );
    polls.ticks().await;
    assert_eq!(next(&mut rx, Duration::from_millis(400)).await, None);
    task.abort();
}

/// The line arrives after the grace period (a delayed reader thread): the kill was already reported
/// unnamed and the line must not make a second report, nor leave a credit that swallows the next
/// counted kill.
#[tokio::test]
async fn a_log_line_after_its_unnamed_report_is_not_reported_again() {
    let fake = Fake::new("late", 0);
    let (tx, mut rx) = mpsc::channel(8);
    let (task, mut polls) = Polls::start(fake.sources(), tx);
    polls.baseline().await;
    fake.set_counter(1);
    assert_eq!(
        next(&mut rx, Duration::from_secs(2)).await,
        Some(AgentMessage::oom_kill(None, None))
    );
    fake.log("3,3,3,-;Out of memory: Killed process 8 (hog) total-vm:9kB\n");
    polls.blocking_until_log_kill_queued();
    polls.ticks().await;
    assert_eq!(next(&mut rx, Duration::from_millis(300)).await, None);
    // The next kill, whose line is lost, is still reported.
    fake.set_counter(2);
    assert_eq!(
        next(&mut rx, Duration::from_secs(2)).await,
        Some(AgentMessage::oom_kill(None, None))
    );
    task.abort();
}

#[tokio::test]
async fn watch_works_from_the_log_alone_and_gives_up_with_no_source() {
    let fake = Fake::new("logonly", 0);
    let mut sources = fake.sources();
    sources.vmstat = fake.dir.join("missing");
    let (tx, mut rx) = mpsc::channel(8);
    let (task, mut polls) = Polls::start(sources.clone(), tx);
    // The log is opened before the first reading.
    polls.baseline().await;
    fake.log("3,3,3,-;Out of memory: Killed process 7 (x) total-vm:9kB\n");
    assert_eq!(
        next(&mut rx, Duration::from_secs(2)).await,
        Some(AgentMessage::oom_kill(Some(7), Some("x".into())))
    );
    task.abort();

    sources.kmsg = fake.dir.join("missing-too");
    let (tx, _rx) = mpsc::channel(8);
    tokio::time::timeout(Duration::from_secs(2), watch(sources, tx))
        .await
        .expect("watch with no source returns");
}

#[test]
fn a_full_queue_drops_and_a_closed_one_stops() {
    let (tx, rx) = mpsc::channel(1);
    assert!(send(&tx, AgentMessage::oom_kill(None, None)));
    assert!(send(&tx, AgentMessage::oom_kill(None, None)));
    drop(rx);
    assert!(!send(&tx, AgentMessage::oom_kill(None, None)));
}

/// A log that plays back a script of reads, then reports end of file.
struct ScriptedLog(VecDeque<io::Result<Vec<u8>>>);

impl Read for ScriptedLog {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.pop_front() {
            Some(Ok(bytes)) => {
                buf[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Some(Err(err)) => Err(err),
            None => Ok(0),
        }
    }
}

const KILL_RECORD: &[u8] = b"3,3,3,-;Out of memory: Killed process 9 (hog) total-vm:9kB\n";

fn scripted(steps: Vec<io::Result<Vec<u8>>>) -> ScriptedLog {
    ScriptedLog(steps.into())
}

#[test]
fn the_reader_goes_on_after_records_were_lost_to_an_overrun() {
    let (tx, mut rx) = mpsc::channel(KILL_QUEUE);
    let log = scripted(vec![
        Err(io::ErrorKind::BrokenPipe.into()),
        Ok(KILL_RECORD.to_vec()),
    ]);
    let reader = std::thread::spawn(move || read_log(log, Duration::from_millis(1), &tx, || {}));
    assert_eq!(rx.blocking_recv(), kill(9, "hog"));
    // End of file with the queue's receiver gone: the thread ends.
    drop(rx);
    reader.join().unwrap();
}

#[test]
fn the_reader_stops_on_any_other_read_error() {
    let (tx, mut rx) = mpsc::channel(KILL_QUEUE);
    let log = scripted(vec![
        Err(io::ErrorKind::PermissionDenied.into()),
        Ok(KILL_RECORD.to_vec()),
    ]);
    std::thread::spawn(move || read_log(log, Duration::from_millis(1), &tx, || {}))
        .join()
        .unwrap();
    // It sent nothing and dropped its sender.
    assert_eq!(rx.blocking_recv(), None);
}

#[test]
fn the_reader_ends_when_the_queue_closes_while_it_waits_for_records() {
    let (tx, rx) = mpsc::channel(KILL_QUEUE);
    drop(rx);
    // Nothing to read, receiver gone: the end-of-file check ends the thread.
    std::thread::spawn(move || read_log(scripted(vec![]), Duration::from_millis(1), &tx, || {}))
        .join()
        .unwrap();
}

#[test]
fn the_reader_ends_when_it_cannot_queue_a_kill() {
    let (tx, rx) = mpsc::channel(KILL_QUEUE);
    drop(rx);
    let log = scripted(vec![Ok(KILL_RECORD.to_vec()), Ok(KILL_RECORD.to_vec())]);
    std::thread::spawn(move || read_log(log, Duration::from_secs(60), &tx, || {}))
        .join()
        .unwrap();
}

#[test]
fn the_reader_reports_each_queued_kill_to_its_observer() {
    let (tx, mut rx) = mpsc::channel(KILL_QUEUE);
    let log = scripted(vec![Ok(KILL_RECORD.to_vec())]);
    let (queued_tx, queued) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        read_log(log, Duration::from_millis(1), &tx, move || {
            let _ = queued_tx.send(());
        });
    });
    assert_eq!(rx.blocking_recv(), kill(9, "hog"));
    queued.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(rx);
    reader.join().unwrap();
}
