// SPDX-License-Identifier: AGPL-3.0-or-later
//! Updates of the running server.

use super::*;

// Updates.

const NEWER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const NEWER_RELEASE: &str = "1.144.0";

/// Looks every 100 ms, so a test waits for what it needs instead of for a day.
fn quick() -> UpdateTimes {
    let step = Duration::from_millis(100);
    UpdateTimes {
        first: step,
        every: step,
        retry: step,
    }
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// Stands in for the server accepting connections: the agent counts a build as started once its
/// port answers. Bound after the fake server started, dropped before the next one is expected.
fn accepting(port: u16) -> std::net::TcpListener {
    std::net::TcpListener::bind(("127.0.0.1", port)).unwrap()
}

fn pointer(builds: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(builds.join(name)).ok()
}

struct Running {
    f: Fixture,
    service: &'static Service,
    control: std::sync::Arc<Control>,
    builds: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

/// Starts the agent's editor with the first build, a server that waits.
async fn running(f: Fixture, first_server_crashes: bool) -> Running {
    let service = leak(Service::honest(server_tarball(
        &f.out,
        COMMIT,
        first_server_crashes,
    )));
    let control = Control::with_times(&f.config.server, quick());
    let builds = PathBuf::from(&f.config.server.dir);
    let task = tokio::spawn(editor::run_controlled(
        f.config.clone(),
        service,
        control.clone(),
    ));
    eventually("the first start", WAIT, || f.out.join("1.pid").exists()).await;
    Running {
        f,
        service,
        control,
        builds,
        task,
    }
}

impl Running {
    /// The first build has started and accepted a connection; returns what stands in for it.
    async fn confirmed(&self) -> std::net::TcpListener {
        let listener = accepting(self.f.config.server.port);
        eventually("the first build to be confirmed", WAIT, || {
            pointer(&self.builds, "confirmed").as_deref() == Some(COMMIT)
        })
        .await;
        listener
    }

    fn publish_newer(&self, crashes: bool) {
        self.service.publish(
            NEWER,
            NEWER_RELEASE,
            server_tarball(&self.f.out, NEWER, crashes),
        );
    }

    /// Ends the task; the dropped child takes the fake server with it.
    fn end(self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn a_newer_build_is_staged_beside_the_running_one_and_runs_only_after_the_switch() {
    let r = running(fixture(false), false).await;
    let listener = r.confirmed().await;
    let first_pid = read(&r.f.out, "1.pid");
    r.publish_newer(false);
    eventually("the newer build to be staged", WAIT, || {
        pointer(&r.builds, "staged").as_deref() == Some(NEWER)
    })
    .await;
    // Beside, not instead: the running build, its pointer and its process are untouched.
    assert_eq!(
        pointer(&r.builds, "current").as_deref(),
        Some(COMMIT),
        "staging must not change what runs"
    );
    assert!(alive(&first_pid) && !r.f.out.join("2.pid").exists());
    assert!(r.builds.join(NEWER).join("bin/code-server").is_file());
    assert!(r.builds.join(COMMIT).is_dir());

    // The switch: the old server is ended, the new build is the one that starts.
    assert_eq!(r.control.switch().unwrap().as_deref(), Some(NEWER));
    drop(listener);
    eventually("the newer build to start", WAIT, || {
        r.f.out.join("2.pid").exists()
    })
    .await;
    assert_eq!(read(&r.f.out, "2.build").trim(), NEWER);
    assert!(!alive(&first_pid), "the old server was ended");
    // Until the new build has accepted a connection, the old one is kept.
    assert_eq!(pointer(&r.builds, "previous").as_deref(), Some(COMMIT));
    assert!(r.builds.join(COMMIT).is_dir());
    assert!(pointer(&r.builds, "staged").is_none());
    // The same token: a window that was signed in need not sign in again.
    assert_eq!(read(&r.f.out, "2.token"), read(&r.f.out, "1.token"));

    let _listener = accepting(r.f.config.server.port);
    eventually("the previous build to be removed", WAIT, || {
        !r.builds.join(COMMIT).exists()
    })
    .await;
    assert_eq!(names(&r.builds), [NEWER, "confirmed", "current"]);
    assert_eq!(pointer(&r.builds, "confirmed").as_deref(), Some(NEWER));
    // Nothing more was asked of Microsoft than what looking for updates needs.
    assert_eq!(r.service.downloads.load(Ordering::SeqCst), 2);
    r.end();
}

#[tokio::test]
async fn a_new_build_that_exits_before_it_started_is_dropped_for_the_previous_one() {
    let r = running(fixture(false), false).await;
    let listener = r.confirmed().await;
    r.publish_newer(true);
    eventually("the newer build to be staged", WAIT, || {
        pointer(&r.builds, "staged").is_some()
    })
    .await;
    assert!(r.control.switch().unwrap().is_some());
    drop(listener);
    // The newer build starts, exits at once, and the first build is started again.
    eventually("the first build to run again", WAIT, || {
        r.f.out.join("3.pid").exists()
    })
    .await;
    assert_eq!(read(&r.f.out, "2.build").trim(), NEWER);
    assert_eq!(read(&r.f.out, "3.build").trim(), COMMIT);
    assert_eq!(pointer(&r.builds, "current").as_deref(), Some(COMMIT));
    assert_eq!(pointer(&r.builds, "rejected").as_deref(), Some(NEWER));
    // The failed build's folder goes after the pointers move: wait for it, never race it.
    eventually("the failed build to be removed", WAIT, || {
        !r.builds.join(NEWER).exists()
    })
    .await;
    assert_eq!(
        names(&r.builds),
        ["confirmed", "current", COMMIT, "rejected"]
    );
    // The failed build is not fetched or staged again while it is still the newest.
    let downloads = r.service.downloads.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(r.service.downloads.load(Ordering::SeqCst), downloads);
    assert!(pointer(&r.builds, "staged").is_none());
    r.end();
}

#[tokio::test]
async fn nothing_is_looked_for_or_switched_with_automatic_updates_off() {
    let r = running(
        fixture_updating(MicrosoftOptions {
            telemetry: false,
            auto_update: false,
        }),
        false,
    )
    .await;
    r.publish_newer(false);
    // An absence window: several update intervals (`quick`) pass, and nothing may have been asked.
    tokio::time::sleep(Duration::from_millis(600)).await;
    // Only the first install's three requests: no look, no download.
    assert_eq!(r.service.requests(), 3);
    assert!(pointer(&r.builds, "staged").is_none());
    // A build left staged by an earlier run (the setting was on then) is not run either.
    std::fs::write(r.builds.join("staged"), NEWER).unwrap();
    assert_eq!(r.control.switch().unwrap(), None);
    assert!(!r.f.out.join("2.pid").exists());
    r.end();
}

#[tokio::test]
async fn a_failed_look_is_retried_and_the_build_is_staged_when_the_service_answers() {
    let r = running(fixture(false), false).await;
    let _listener = r.confirmed().await;
    r.service.failing.store(3, Ordering::SeqCst);
    r.publish_newer(false);
    eventually("the newer build to be staged after failures", WAIT, || {
        pointer(&r.builds, "staged").as_deref() == Some(NEWER)
    })
    .await;
    // The running server never noticed.
    assert_eq!(pointer(&r.builds, "current").as_deref(), Some(COMMIT));
    assert!(!r.f.out.join("2.pid").exists());
    r.end();
}

#[tokio::test]
async fn a_switch_with_nothing_staged_leaves_the_server_running() {
    let r = running(fixture(false), false).await;
    let _listener = r.confirmed().await;
    assert_eq!(r.control.switch().unwrap(), None);
    // An absence window: a restart, had the switch caused one, would have started a second server by now.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!r.f.out.join("2.pid").exists());
    assert!(alive(&read(&r.f.out, "1.pid")));
    r.end();
}

/// A builds folder with the first build installed and current.
async fn installed(dir: &tempfile::TempDir) -> (PathBuf, &'static Service) {
    let service = leak(Service::honest(good_tarball()));
    ensure_build(&service, PLATFORM, &builds(dir))
        .await
        .unwrap();
    (builds(dir), service)
}

#[tokio::test]
async fn looking_for_updates_without_a_current_build_asks_nothing_and_stages_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let service = leak(Service::honest(good_tarball()));
    let builds = builds(&dir);
    assert_eq!(
        stage_update(&service, PLATFORM, &builds).await.unwrap(),
        Update::UpToDate
    );
    assert_eq!(service.requests(), 0);
    assert!(!builds.exists() || names(&builds).is_empty());
}

#[tokio::test]
async fn looking_for_updates_downloads_only_a_build_that_is_neither_current_nor_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    let before = service.downloads.load(Ordering::SeqCst);
    // The newest is the current one: one question, no download.
    assert_eq!(
        stage_update(&service, PLATFORM, &builds).await.unwrap(),
        Update::UpToDate
    );
    assert_eq!(service.downloads.load(Ordering::SeqCst), before);
    // A newer one is downloaded, named as staged and is not yet current.
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    assert_eq!(
        stage_update(&service, PLATFORM, &builds).await.unwrap(),
        Update::Staged(NEWER.to_owned())
    );
    assert_eq!(current_build(&builds), Some(builds.join(COMMIT)));
    assert_eq!(names(&builds), [NEWER, "current", COMMIT, "staged"]);
    // Asking again finds the build complete and downloads nothing more.
    let downloads = service.downloads.load(Ordering::SeqCst);
    stage_update(&service, PLATFORM, &builds).await.unwrap();
    assert_eq!(service.downloads.load(Ordering::SeqCst), downloads);
    // A build that failed to start here is not staged again.
    std::fs::remove_file(builds.join("staged")).unwrap();
    std::fs::write(builds.join("rejected"), NEWER).unwrap();
    assert_eq!(
        stage_update(&service, PLATFORM, &builds).await.unwrap(),
        Update::UpToDate
    );
}

/// A service whose download of the build `NEWER` runs `during` once the body is there, as if the
/// rest of the agent had acted while the bytes were still coming in.
struct SlowDownload {
    service: &'static Service,
    during: Box<dyn Fn() + Send + Sync>,
}

impl Fetch for SlowDownload {
    async fn text(&self, url: &str) -> io::Result<String> {
        (&self.service).text(url).await
    }

    async fn file(&self, url: &str, dest: &Path, max_bytes: u64) -> io::Result<()> {
        (&self.service).file(url, dest, max_bytes).await?;
        (self.during)();
        Ok(())
    }
}

#[tokio::test]
async fn a_build_rolled_back_while_it_downloads_is_not_staged() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    let during = {
        let builds = builds.clone();
        move || {
            // The build became current and failed to start while the download ran.
            std::fs::write(builds.join("previous"), COMMIT).unwrap();
            std::fs::write(builds.join("current"), NEWER).unwrap();
            assert!(roll_back(&builds, NEWER, false).unwrap());
        }
    };
    let fetch = SlowDownload {
        service,
        during: Box::new(during),
    };
    assert_eq!(
        stage_update(&fetch, PLATFORM, &builds).await.unwrap(),
        Update::UpToDate
    );
    assert_eq!(pointer(&builds, "staged"), None);
    assert_eq!(switch_to_staged(&builds).unwrap(), None);
    assert_eq!(current_build(&builds), Some(builds.join(COMMIT)));
}

#[tokio::test]
async fn a_download_that_is_not_the_published_file_stages_nothing_and_touches_no_build() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    service.publish_as(
        NEWER,
        NEWER_RELEASE,
        &sha(b"something else"),
        good_tarball(),
    );
    let err = stage_update(&service, PLATFORM, &builds).await.unwrap_err();
    assert!(matches!(err, InstallError::Checksum { .. }), "{err}");
    assert_eq!(names(&builds), ["current", COMMIT]);
    assert_eq!(switch_to_staged(&builds).unwrap(), None);
    assert_eq!(current_build(&builds), Some(builds.join(COMMIT)));
}

#[tokio::test]
async fn a_staged_name_without_a_complete_build_is_dropped_not_run() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, _service) = installed(&dir).await;
    std::fs::write(builds.join("staged"), NEWER).unwrap();
    assert_eq!(switch_to_staged(&builds).unwrap(), None);
    assert!(!builds.join("staged").exists());
    // A folder with no marker is a half-made build, not a complete one.
    std::fs::create_dir_all(builds.join(NEWER).join("bin")).unwrap();
    std::fs::write(builds.join(NEWER).join("bin/code-server"), "x").unwrap();
    std::fs::write(builds.join("staged"), NEWER).unwrap();
    assert_eq!(switch_to_staged(&builds).unwrap(), None);
    // The build that is current stays current, and nothing is on trial.
    assert_eq!(current_build(&builds), Some(builds.join(COMMIT)));
    assert!(!on_trial(&builds));
}

#[tokio::test]
async fn the_first_build_has_nothing_to_go_back_to_and_a_confirmed_build_is_never_rolled_back() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    // The first install: no previous build, so no trial and no rollback.
    assert!(!on_trial(&builds));
    assert!(!roll_back(&builds, COMMIT, true).unwrap());
    confirm(&builds, COMMIT).unwrap();
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    stage_update(&service, PLATFORM, &builds).await.unwrap();
    assert_eq!(switch_to_staged(&builds).unwrap().as_deref(), Some(NEWER));
    assert!(on_trial(&builds));
    // The old build's late confirmation or failure (its server ended after the switch) changes
    // nothing: it is not the current build any more, and it is still kept.
    confirm(&builds, COMMIT).unwrap();
    assert!(on_trial(&builds));
    assert!(!roll_back(&builds, COMMIT, true).unwrap());
    assert!(builds.join(COMMIT).is_dir());
    // Confirmed: the trial is over and the old build is gone.
    confirm(&builds, NEWER).unwrap();
    assert!(!on_trial(&builds));
    assert!(!roll_back(&builds, NEWER, true).unwrap());
    assert_eq!(names(&builds), [NEWER, "confirmed", "current"]);
    // With nothing current at all, confirming is a no-op.
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    confirm(&empty, COMMIT).unwrap();
    assert_eq!(names(&empty), Vec::<String>::new());
}

#[tokio::test]
async fn a_clean_up_leaves_files_it_does_not_know_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, _service) = installed(&dir).await;
    std::fs::write(builds.join("notes.txt"), "mine").unwrap();
    std::fs::write(
        builds.join(format!("{NEWER}.tar.gz.partial")),
        "dead download",
    )
    .unwrap();
    std::fs::create_dir(builds.join("somebody-elses-folder")).unwrap();
    confirm(&builds, COMMIT).unwrap();
    assert_eq!(
        names(&builds),
        [
            "confirmed",
            "current",
            COMMIT,
            "notes.txt",
            "somebody-elses-folder"
        ]
    );
}

#[tokio::test]
async fn a_current_build_that_is_gone_is_replaced_by_the_staged_one_with_nothing_to_go_back_to() {
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    stage_update(&service, PLATFORM, &builds).await.unwrap();
    std::fs::remove_dir_all(builds.join(COMMIT)).unwrap();
    assert_eq!(switch_to_staged(&builds).unwrap().as_deref(), Some(NEWER));
    assert_eq!(current_build(&builds), Some(builds.join(NEWER)));
    // The build that was current is not complete, so it is no build to go back to.
    assert!(!on_trial(&builds));
    assert!(!builds.join("previous").exists());
}

/// Makes `path` unwritable for the test's user (root ignores the mode, so there it does nothing).
fn lock(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
}

fn unlock(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn is_root() -> bool {
    let probe = tempfile::tempdir().unwrap();
    lock(probe.path());
    let writable = std::fs::write(probe.path().join("x"), "").is_ok();
    unlock(probe.path());
    writable
}

#[tokio::test]
async fn an_old_build_that_cannot_be_removed_stays_and_the_clean_up_goes_on() {
    if is_root() {
        // skip-ok: root ignores a folder's mode, so nothing can fail to be removed; CI runs as a user
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (builds, _service) = installed(&dir).await;
    let stuck = builds.join(NEWER);
    std::fs::create_dir_all(stuck.join("inner")).unwrap();
    std::fs::write(stuck.join("inner/file"), "x").unwrap();
    lock(&stuck.join("inner"));
    let leftover = builds.join(format!("{NEWER}.tar.gz.partial"));
    std::fs::write(&leftover, "dead download").unwrap();
    confirm(&builds, COMMIT).unwrap();
    unlock(&stuck.join("inner"));
    assert!(
        stuck.join("inner/file").is_file(),
        "the folder is still there"
    );
    assert!(!leftover.exists(), "the other leftover went all the same");
    assert!(builds.join(COMMIT).is_dir());
}

fn control_for(builds: &Path) -> std::sync::Arc<Control> {
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
    Control::new(&server)
}

#[tokio::test]
async fn the_host_asking_for_a_switch_gets_whether_a_newer_build_now_runs() {
    use puddle_agent::host_control::reply_to;
    use puddle_agent_proto::host_control::{HostMessage, HostReply};
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    let ask = |control| reply_to(HostMessage::SwitchEditorBuild, &[], control);
    // A server that is not kept up to date has no control: the message is not for it.
    assert_eq!(ask(None).await, HostReply::Ignored);
    let control = control_for(&builds);
    assert_eq!(
        ask(Some(&control)).await,
        HostReply::EditorBuild { switched: false }
    );
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    stage_update(&service, PLATFORM, &builds).await.unwrap();
    assert_eq!(
        ask(Some(&control)).await,
        HostReply::EditorBuild { switched: true }
    );
    assert_eq!(current_build(&builds), Some(builds.join(NEWER)));
}

#[tokio::test]
async fn a_switch_that_cannot_be_recorded_is_answered_as_not_switched() {
    use puddle_agent::host_control::reply_to;
    use puddle_agent_proto::host_control::{HostMessage, HostReply};
    if is_root() {
        // skip-ok: root ignores a folder's mode, so nothing can fail to be removed; CI runs as a user
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (builds, service) = installed(&dir).await;
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    stage_update(&service, PLATFORM, &builds).await.unwrap();
    let control = control_for(&builds);
    lock(&builds);
    let reply = reply_to(HostMessage::SwitchEditorBuild, &[], Some(&control)).await;
    unlock(&builds);
    assert_eq!(reply, HostReply::EditorBuild { switched: false });
    assert_eq!(current_build(&builds), Some(builds.join(COMMIT)));
}
