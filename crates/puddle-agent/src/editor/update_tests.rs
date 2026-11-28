// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests of the update schedule, refused downloads and rollbacks.

use futures_util::FutureExt as _;
use puddle_agent_proto::editor::MicrosoftOptions;
use tokio::sync::Notify;

use super::*;

#[test]
fn a_switch_made_before_the_server_reads_its_build_is_not_a_restart_request() {
    let dir = tempfile::tempdir().unwrap();
    let builds = dir.path().join("servers");
    let (old, new) = ("1".repeat(40), "2".repeat(40));
    for commit in [&old, &new] {
        let program = builds.join(commit).join(microsoft::PROGRAM);
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "").unwrap();
        std::fs::write(builds.join(commit).join(".puddle-installed"), "").unwrap();
    }
    std::fs::write(builds.join("current"), &old).unwrap();
    let mut server = EditorServer::microsoft(
        "/d",
        "/f",
        120,
        MicrosoftOptions {
            telemetry: false,
            auto_update: true,
        },
    );
    server.dir = builds.to_string_lossy().into_owned();
    let control = Control::new(&server);
    assert_eq!(control.claim(), Some((builds.join(&old), false)));
    // Staged and switched while no server runs: the next read gets the new build, on trial,
    // and the request that switch made is spent by that read, not left to stop the server
    // that is about to start.
    std::fs::write(builds.join("staged"), &new).unwrap();
    assert_eq!(control.switch().unwrap(), Some(new.clone()));
    assert_eq!(control.claim(), Some((builds.join(&new), true)));
    assert!(control.restart.notified().now_or_never().is_none());
    // A switch after the read is a request the running server answers.
    std::fs::write(builds.join("staged"), &old).unwrap();
    assert_eq!(control.switch().unwrap(), Some(old));
    assert!(control.restart.notified().now_or_never().is_some());
}

#[test]
fn restarts_wait_longer_after_each_quick_exit_up_to_a_limit() {
    let waits: Vec<u64> = [1, 2, 3, 10, 30, 31, 1000]
        .into_iter()
        .map(|n| wait_after(n).as_secs())
        .collect();
    assert_eq!(waits, [1, 2, 3, 10, 30, 30, 30]);
    assert_eq!(wait_after(0).as_secs(), 1);
}

#[test]
fn a_refused_download_waits_a_minute_then_doubles_up_to_an_hour() {
    let waits: Vec<u64> = [0, 1, 2, 3, 6, 7, 8, 1000, u32::MAX]
        .into_iter()
        .map(|n| wait_after_refusal(n).as_secs())
        .collect();
    assert_eq!(waits, [60, 60, 120, 240, 1920, 3600, 3600, 3600, 3600]);
}

/// An update service that answers every metadata request with one build and a published hash
/// that is not the tarball's, and counts the downloads.
struct WrongHash {
    downloads: std::sync::atomic::AtomicUsize,
}

impl Fetch for Arc<WrongHash> {
    async fn text(&self, url: &str) -> io::Result<String> {
        tokio::task::yield_now().await; // a network answer is not instant
        let commit = "e".repeat(40);
        Ok(if url.contains("/api/latest/") {
            format!(r#"{{"version":"{commit}","productVersion":"1.0.0"}}"#)
        } else {
            format!(
                r#"{{"version":"{commit}","sha256hash":"{}"}}"#,
                "0".repeat(64)
            )
        })
    }

    async fn file(&self, _url: &str, dest: &Path, _max_bytes: u64) -> io::Result<()> {
        tokio::task::yield_now().await; // a download is not instant
        self.downloads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(dest, b"not the published file")
    }
}

/// A Microsoft server config on a fresh volume, and the service that refuses every build.
fn refusing(dir: &Path, auto_update: bool) -> (EditorConfig, Arc<WrongHash>) {
    let mut server = EditorServer::microsoft(
        "/d",
        "/f",
        120,
        MicrosoftOptions {
            telemetry: false,
            auto_update,
        },
    );
    server.dir = dir.join("servers").to_string_lossy().into_owned();
    server.data = dir.join("data").to_string_lossy().into_owned();
    let config = EditorConfig {
        server,
        password_file: dir.join("password"),
        boot_id: dir.join("boot_id"),
        root: dir.join("root"),
    };
    let service = Arc::new(WrongHash {
        downloads: std::sync::atomic::AtomicUsize::new(0),
    });
    (config, service)
}

/// A build folder `commit` that `microsoft` takes for complete.
fn complete_build(builds: &Path, commit: &str) {
    let dir = builds.join(commit);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join(microsoft::PROGRAM), b"").unwrap();
    std::fs::write(dir.join(".puddle-installed"), b"").unwrap();
}

/// The update loop holds `work` for a whole download (hundreds of MB), and the server's restart
/// after a failed new build must not wait for it.
#[tokio::test]
async fn the_rollback_after_a_failed_new_build_does_not_wait_for_a_running_download() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), true);
    let control = Control::new(&config.server);
    let (old, new) = ("a".repeat(40), "b".repeat(40));
    let builds = &control.builds;
    std::fs::create_dir_all(builds).unwrap();
    complete_build(builds, &old);
    complete_build(builds, &new);
    std::fs::write(builds.join("previous"), &old).unwrap();
    std::fs::write(builds.join("current"), &new).unwrap();
    // The folder of the download that is running: a clean-up now would delete it.
    let partial = builds.join(format!("{}.partial", "c".repeat(40)));
    std::fs::create_dir(&partial).unwrap();

    let download = control.work.lock().await;
    // A rollback that waited would block here for good, as the download never ends.
    settle_trial(&control, &new);
    assert_eq!(
        std::fs::read_to_string(builds.join("current")).unwrap(),
        old,
        "the previous build runs again"
    );
    assert!(
        partial.exists(),
        "the running download's folder is not cleaned up"
    );
    drop(download);

    // With no download running, the clean-up goes with the rollback as before.
    std::fs::write(builds.join("previous"), &old).unwrap();
    std::fs::write(builds.join("current"), &new).unwrap();
    std::fs::remove_file(builds.join("rejected")).unwrap();
    settle_trial(&control, &new);
    assert!(!partial.exists());
    assert!(!builds.join(&new).exists());
}

/// An update service whose first answer is held until `open` is notified: a download in flight.
struct Held {
    open: Arc<Notify>,
}

impl Fetch for Held {
    async fn text(&self, _url: &str) -> io::Result<String> {
        self.open.notified().await;
        Err(io::Error::other("no route to host"))
    }

    async fn file(&self, _url: &str, _dest: &Path, _max_bytes: u64) -> io::Result<()> {
        self.open.notified().await;
        Err(io::Error::other("no route to host"))
    }
}

/// A failed build whose rollback came while a download held `work` still has its folder removed:
/// the download's holder cleans up when it lets go, not some later confirmation.
#[tokio::test(start_paused = true)]
async fn the_failed_builds_folder_is_removed_when_the_download_that_blocked_its_clean_up_ends() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), true);
    let control = Control::with_times(&config.server, times(Duration::from_mins(1)));
    let (old, new) = ("a".repeat(40), "b".repeat(40));
    let builds = Path::new(&config.server.dir);
    std::fs::create_dir_all(builds).unwrap();
    complete_build(builds, &old);
    complete_build(builds, &new);
    std::fs::write(builds.join("previous"), &old).unwrap();
    std::fs::write(builds.join("current"), &new).unwrap();
    let open = Arc::new(Notify::new());
    let looking = tokio::spawn(update_loop(
        Arc::clone(&control),
        Arc::new(Held {
            open: Arc::clone(&open),
        }),
    ));
    // The first look is under way and holds `work`.
    tokio::time::sleep(Duration::from_secs(5)).await;
    settle_trial(&control, &new);
    assert_eq!(
        std::fs::read_to_string(builds.join("current")).unwrap(),
        old
    );
    assert!(builds.join(&new).exists(), "not while the download runs");
    open.notify_one();
    puddle_test_kit::eventually(
        "the failed build to be removed",
        Duration::from_secs(30),
        || !builds.join(&new).exists(),
    )
    .await;
    assert!(builds.join(&old).exists());
    looking.abort();
}

#[tokio::test(start_paused = true)]
async fn a_download_with_the_wrong_hash_is_fetched_on_a_slowing_schedule_not_every_30_seconds() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), false);
    let running = tokio::spawn(run_with(config, Arc::clone(&service)));
    // Downloads at 0, 1, 3, 7, 15, 31 and 63 minutes; the next is at 123. Every 30 seconds
    // would be 200.
    tokio::time::sleep(Duration::from_mins(100)).await;
    assert_eq!(service.downloads.load(SeqCst), 7);
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn every_refused_download_is_told_to_the_host_in_words_without_checksums() {
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), false);
    let control = Control::new(&config.server);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let running = tokio::spawn(run_controlled(config, service, Arc::clone(&control)));
    let wanted = AgentMessage::editor_status(
        EditorProblemKind::Install,
        Some("the download does not match the checksum Microsoft published for it"),
    );
    // The first refusal, then the next one a minute later: a host that missed the first
    // (it restarted) hears the second.
    assert_eq!(host.recv().await, Some(wanted.clone()));
    assert_eq!(host.recv().await, Some(wanted));
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn an_install_that_works_ends_the_report_once_and_says_nothing_when_nothing_was_reported() {
    let control = Control::new(&EditorServer::microsoft(
        "/d",
        "/f",
        120,
        MicrosoftOptions {
            telemetry: false,
            auto_update: false,
        },
    ));
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    control.install_worked();
    puddle_test_kit::no_event_for(
        "a message when nothing was wrong",
        Duration::from_secs(1),
        &mut host,
    )
    .await;
    control.install_failed("nope");
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::Install,
            Some("nope")
        ))
    );
    control.install_worked();
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::Install,
            None
        ))
    );
    control.install_worked();
    puddle_test_kit::no_event_for("a second end", Duration::from_secs(1), &mut host).await;
}

#[tokio::test(start_paused = true)]
async fn a_server_that_runs_as_root_for_want_of_its_user_is_told_each_start_and_ends_when_it_is_found()
 {
    let agent = Account::find("agent", "agent:x:1001:1001::/home/agent:/bin/sh\n", "").unwrap();
    let named = EditorServer::new("/d", "/f", 300).with_user(puddle_types::SessionUser::Agent);
    let control = Control::new(&named);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    // Root by choice, and a user that is there, say nothing.
    assert_eq!(
        account(
            &EditorServer::new("/d", "/f", 300),
            &control,
            |_| unreachable!()
        ),
        None
    );
    assert_eq!(
        account(&named, &control, |name| {
            assert_eq!(name, "agent");
            Ok(agent.clone())
        }),
        Some(agent.clone())
    );
    puddle_test_kit::no_event_for("a user that is there", Duration::from_secs(1), &mut host).await;
    // A user the guest lacks: the server runs as root, and the host hears why at each start.
    let lacking = |_: &str| Err(io::Error::new(io::ErrorKind::NotFound, "no entry"));
    let wanted = AgentMessage::editor_status(
        EditorProblemKind::SessionUser,
        Some(
            "the session user agent is missing in the guest (no entry), so the editor server runs as root",
        ),
    );
    assert_eq!(account(&named, &control, lacking), None);
    assert_eq!(host.recv().await, Some(wanted.clone()));
    assert_eq!(account(&named, &control, lacking), None);
    assert_eq!(host.recv().await, Some(wanted));
    // A start that finds the user ends it, once.
    assert_eq!(
        account(&named, &control, |_| Ok(agent.clone())),
        Some(agent.clone())
    );
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::SessionUser,
            None
        ))
    );
    assert_eq!(
        account(&named, &control, |_| Ok(agent.clone())),
        Some(agent)
    );
    puddle_test_kit::no_event_for("a second end", Duration::from_secs(1), &mut host).await;
    // Root by choice after a lacking start ends it too.
    assert_eq!(account(&named, &control, lacking), None);
    assert!(host.recv().await.is_some());
    assert_eq!(
        account(
            &EditorServer::new("/d", "/f", 300),
            &control,
            |_| unreachable!()
        ),
        None
    );
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::SessionUser,
            None
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn a_restart_request_ends_the_wait_after_a_refused_download() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), false);
    let control = Control::new(&config.server);
    let running = tokio::spawn(run_controlled(
        config,
        Arc::clone(&service),
        Arc::clone(&control),
    ));
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(service.downloads.load(SeqCst), 1);
    control.restart.notify_one();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(service.downloads.load(SeqCst), 2, "tried again at once");
    running.abort();
}

/// Marks a build as the one in use, so that looking for an update has something to update (it
/// does nothing before the first install).
fn has_a_current_build(config: &EditorConfig) {
    let builds = Path::new(&config.server.dir);
    std::fs::create_dir_all(builds).unwrap();
    std::fs::write(builds.join("current"), "a".repeat(40)).unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_refused_update_is_looked_for_on_a_slowing_schedule_too() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), true);
    has_a_current_build(&config);
    let times = UpdateTimes {
        first: Duration::from_secs(1),
        every: Duration::from_hours(24),
        retry: Duration::from_mins(1),
    };
    let control = Control::with_times(&config.server, times);
    let looking = tokio::spawn(update_loop(control, Arc::new(Arc::clone(&service))));
    // Looks at 1 s and then after 1, 2, 4, 8, 16 and 32 minutes (7 in all); every minute would be 100.
    tokio::time::sleep(Duration::from_mins(100)).await;
    assert_eq!(service.downloads.load(SeqCst), 7);
    looking.abort();
}

/// An update service that cannot be reached.
pub(super) struct Offline;

impl Fetch for Offline {
    async fn text(&self, _url: &str) -> io::Result<String> {
        tokio::task::yield_now().await; // a network answer is not instant
        Err(io::Error::other("no route to host"))
    }

    async fn file(&self, _url: &str, _dest: &Path, _max_bytes: u64) -> io::Result<()> {
        tokio::task::yield_now().await;
        Err(io::Error::other("no route to host"))
    }
}

fn times(retry: Duration) -> UpdateTimes {
    UpdateTimes {
        first: Duration::from_secs(1),
        every: Duration::from_hours(24),
        retry,
    }
}

fn update_message(reason: Option<&str>) -> AgentMessage {
    AgentMessage::editor_status(EditorProblemKind::Update, reason)
}

#[tokio::test(start_paused = true)]
async fn a_refused_newer_build_is_told_as_an_update_problem_until_a_look_works() {
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), true);
    has_a_current_build(&config);
    let control = Control::with_times(&config.server, times(Duration::from_mins(1)));
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let looking = tokio::spawn(update_loop(control, Arc::new(Arc::clone(&service))));
    assert_eq!(
        host.recv().await,
        Some(update_message(Some(
            "a newer build was refused: the download does not match the checksum Microsoft published for it"
        )))
    );
    // The build is now known to be bad (as after a failed start), so the next look is quiet.
    std::fs::write(dir.path().join("servers/rejected"), "e".repeat(40)).unwrap();
    assert_eq!(host.recv().await, Some(update_message(None)));
    looking.abort();
}

#[tokio::test(start_paused = true)]
async fn looks_that_fail_are_told_only_after_five_in_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), true);
    has_a_current_build(&config);
    let control = Control::with_times(&config.server, times(Duration::from_mins(1)));
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let looking = tokio::spawn(update_loop(control, Arc::new(Offline)));
    // Looks at 1 s, then every minute: four have failed at 200 s.
    puddle_test_kit::no_event_for(
        "a report before the fifth failed look",
        Duration::from_secs(200),
        &mut host,
    )
    .await;
    let Some(AgentMessage::EditorStatus {
        kind: EditorProblemKind::Update,
        problem: Some(reason),
    }) = host.recv().await
    else {
        panic!("no update problem after the fifth failed look");
    };
    assert!(
        reason.starts_with("looking for a newer build keeps failing")
            && reason.contains("no route to host"),
        "{reason}"
    );
    looking.abort();
}

#[tokio::test(start_paused = true)]
async fn a_first_install_that_keeps_failing_on_the_network_is_told_after_five_tries() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), false);
    let control = Control::new(&config.server);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let running = tokio::spawn(run_controlled(config, Offline, Arc::clone(&control)));
    // Tries at 0, 1, 3, 6 and 16 s: four have failed at 10 s.
    puddle_test_kit::no_event_for(
        "a report before the fifth failed try",
        Duration::from_secs(10),
        &mut host,
    )
    .await;
    let Some(AgentMessage::EditorStatus {
        kind: EditorProblemKind::Install,
        problem: Some(reason),
    }) = host.recv().await
    else {
        panic!("no install problem after the fifth failed try");
    };
    assert!(
        reason.starts_with("the download keeps failing") && reason.contains("no route to host"),
        "{reason}"
    );
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn a_build_rolled_back_after_a_bad_start_is_told_and_one_that_stays_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), true);
    let builds = Path::new(&config.server.dir);
    let (old, new) = ("a".repeat(40), "b".repeat(40));
    for commit in [&old, &new] {
        let build = builds.join(commit);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join(".puddle-installed"), b"").unwrap();
        std::fs::write(build.join(microsoft::PROGRAM), b"").unwrap();
    }
    std::fs::write(builds.join("current"), &new).unwrap();
    std::fs::write(builds.join("previous"), &old).unwrap();
    let control = Control::new(&config.server);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    // A build that is not the one on trial is left alone and nothing is said.
    settle_trial(&control, &old);
    puddle_test_kit::no_event_for(
        "a report for no rollback",
        Duration::from_secs(1),
        &mut host,
    )
    .await;
    settle_trial(&control, &new);
    assert_eq!(
        host.recv().await,
        Some(update_message(Some(
            "the newer build exited before it started, so the previous one runs again"
        )))
    );
}
