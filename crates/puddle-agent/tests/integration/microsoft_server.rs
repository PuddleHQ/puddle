// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for Microsoft's editor server: the agent finds the newest build, downloads it, checks
//! the published SHA-256, unpacks it on the volume and starts it with a connection token, the
//! accepted terms and the telemetry choice. A fake update service and a shell script inside a
//! small tarball stand in for Microsoft; `Downloader` is run against fake `curl` and `wget`.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests"
)]

use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::GzEncoder;
use puddle_agent::editor::{self, Control, EditorConfig, UpdateTimes};
use puddle_agent::microsoft::{
    Downloader, Fetch, InstallError, Update, confirm, current_build, download_url, ensure_build,
    install_latest, latest_url, on_trial, roll_back, stage_update, switch_to_staged, versions_url,
};
use puddle_agent_proto::editor::EditorServer;
use sha2::{Digest as _, Sha256};

const WAIT: Duration = Duration::from_secs(30);
const COMMIT: &str = "e646868227c3e3a9e3f24277c8c7220be42c0be7";
const RELEASE: &str = "1.143.1";
const PLATFORM: &str = "server-linux-x64-web";
const TOP: &str = "vscode-server-linux-x64-web";

/// Records what it was started with in `OUT/<start number>.*`, then waits to be killed.
const FAKE_SERVER: &str = r#"#!/bin/sh
n=1
while [ -e "OUT/$n.pid" ]; do n=$((n + 1)); done
printf '%s\n' "$@" >"OUT/$n.args"
printf '%s' "$PASSWORD" >"OUT/$n.password"
cat "$(printf '%s\n' "$@" | sed -n '/^--connection-token-file$/{n;p;}')" >"OUT/$n.token"
echo BUILD >"OUT/$n.build"
echo $$ >"OUT/$n.pid"
RUN
"#;

/// The recording server of the build named `tag`, writing into `out`; `crash` makes it exit at
/// once instead of waiting to be killed.
fn server_script(out: &str, tag: &str, crash: bool) -> String {
    FAKE_SERVER
        .replace("BUILD", tag)
        .replace("RUN", if crash { "exit 3" } else { "exec sleep 600" })
        .replace("OUT", out)
}

/// A tarball holding that server as `bin/code-server`.
fn server_tarball(out: &Path, tag: &str, crash: bool) -> Vec<u8> {
    tarball(&[(
        &format!("{TOP}/bin/code-server"),
        0o755,
        &server_script(&out.to_string_lossy(), tag, crash),
    )])
}

/// A gzipped tarball with the given entries `(path, mode, content)`.
fn tarball(entries: &[(&str, u32, &str)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, mode, content) in entries {
        let mut header = tar::Header::new_gnu();
        // Written into the header as given: `set_path` would refuse the paths some tests need.
        let name = header.as_old_mut();
        name.name = [0; 100];
        name.name
            .get_mut(..path.len())
            .unwrap()
            .copy_from_slice(path.as_bytes());
        header.set_size(content.len() as u64);
        header.set_mode(*mode);
        header.set_entry_type(if path.ends_with('/') {
            tar::EntryType::Directory
        } else {
            tar::EntryType::Regular
        });
        header.set_cksum();
        builder.append(&header, content.as_bytes()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn sha(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The update service's answers, as Microsoft sends them.
fn latest_answer(commit: &str, release: &str) -> String {
    format!(
        r#"{{"name":"{release}","version":"{commit}","productVersion":"{release}","timestamp":1792690529437}}"#
    )
}

fn versions_answer(commit: &str, release: &str, hash: &str) -> String {
    format!(
        r#"{{"url":"https://vscode.download.prss.microsoft.com/dbazure/download/stable/{commit}/vscode-server-linux-x64-web.tar.gz","name":"{release}","version":"{commit}","productVersion":"{release}","hash":"{commit}","timestamp":1792690529437,"sha256hash":"{hash}","supportsFastUpdate":true}}"#
    )
}

/// A fake update service: answers by URL and counts what it was asked.
struct Service {
    answers: Mutex<BTreeMap<String, String>>,
    /// The tarball behind each download URL.
    tarballs: Mutex<BTreeMap<String, Vec<u8>>>,
    texts: AtomicUsize,
    downloads: AtomicUsize,
    /// The first this many requests of any kind fail, like a guest with no network yet.
    failing: AtomicUsize,
    asked: Mutex<Vec<String>>,
}

impl Service {
    /// A service whose newest build is `tarball`, published under `published` (its real hash
    /// unless a test says otherwise).
    fn new(tarball: Vec<u8>, published: &str) -> Self {
        let answers = BTreeMap::from([
            (latest_url(PLATFORM), latest_answer(COMMIT, RELEASE)),
            (
                versions_url(RELEASE, PLATFORM),
                versions_answer(COMMIT, RELEASE, published),
            ),
        ]);
        Self {
            answers: Mutex::new(answers),
            tarballs: Mutex::new(BTreeMap::from([(download_url(COMMIT, PLATFORM), tarball)])),
            texts: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
            failing: AtomicUsize::new(0),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn honest(tarball: Vec<u8>) -> Self {
        let hash = sha(&tarball);
        Self::new(tarball, &hash)
    }

    /// Makes `tarball` the newest build, published honestly as `commit` of `release`.
    fn publish(&self, commit: &str, release: &str, tarball: Vec<u8>) {
        self.publish_as(commit, release, &sha(&tarball), tarball);
    }

    /// [`Service::publish`] with a published hash that need not be the tarball's.
    fn publish_as(&self, commit: &str, release: &str, hash: &str, tarball: Vec<u8>) {
        let mut answers = self.answers.lock().unwrap();
        answers.insert(latest_url(PLATFORM), latest_answer(commit, release));
        answers.insert(
            versions_url(release, PLATFORM),
            versions_answer(commit, release, hash),
        );
        self.tarballs
            .lock()
            .unwrap()
            .insert(download_url(commit, PLATFORM), tarball);
    }

    fn requests(&self) -> usize {
        self.texts.load(Ordering::SeqCst) + self.downloads.load(Ordering::SeqCst)
    }

    fn check_up(&self) -> io::Result<()> {
        let left = self.failing.load(Ordering::SeqCst);
        if left > 0 {
            self.failing.store(left - 1, Ordering::SeqCst);
            return Err(io::Error::other("no network"));
        }
        Ok(())
    }
}

impl Fetch for &'static Service {
    async fn text(&self, url: &str) -> io::Result<String> {
        tokio::task::yield_now().await; // a network answer is not instant
        self.asked.lock().unwrap().push(url.to_owned());
        self.texts.fetch_add(1, Ordering::SeqCst);
        self.check_up()?;
        self.answers
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .ok_or_else(|| io::Error::other(format!("404 {url}")))
    }

    async fn file(&self, url: &str, dest: &Path) -> io::Result<()> {
        tokio::task::yield_now().await; // a download is not instant
        self.asked.lock().unwrap().push(url.to_owned());
        self.downloads.fetch_add(1, Ordering::SeqCst);
        self.check_up()?;
        let tarballs = self.tarballs.lock().unwrap();
        let tarball = tarballs
            .get(url)
            .ok_or_else(|| io::Error::other(format!("404 {url}")))?;
        std::fs::write(dest, tarball)
    }
}

/// Leaks the service: the fetch is `'static`, as the agent's task needs, and a test is short.
fn leak(service: Service) -> &'static Service {
    Box::leak(Box::new(service))
}

fn good_tarball() -> Vec<u8> {
    tarball(&[
        (&format!("{TOP}/"), 0o755, ""),
        (&format!("{TOP}/bin/"), 0o755, ""),
        (
            &format!("{TOP}/bin/code-server"),
            0o755,
            &server_script("@OUT@", "x", false),
        ),
        (&format!("{TOP}/product.json"), 0o644, "{}"),
    ])
}

fn builds(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("data/servers")
}

/// What is in the builds folder, by name.
fn names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

#[tokio::test]
async fn the_newest_build_is_downloaded_checked_unpacked_without_its_top_folder_and_made_current() {
    let dir = tempfile::tempdir().unwrap();
    let service = leak(Service::honest(good_tarball()));
    let build = ensure_build(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap();

    assert_eq!(build, builds(&dir).join(COMMIT));
    assert!(build.join("bin/code-server").is_file());
    assert_eq!(
        std::fs::read_to_string(build.join("product.json")).unwrap(),
        "{}"
    );
    let mode = std::fs::metadata(build.join("bin/code-server"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "the server program stays executable");
    assert_eq!(current_build(&builds(&dir)), Some(build));
    // The tarball and the staging folder are gone; only the build and the pointer are left.
    assert_eq!(names(&builds(&dir)), ["current", COMMIT]);
    // Two answers and one download, asked in the order the contract needs.
    assert_eq!(
        *service.asked.lock().unwrap(),
        [
            latest_url(PLATFORM),
            versions_url(RELEASE, PLATFORM),
            download_url(COMMIT, PLATFORM)
        ]
    );
}

#[tokio::test]
async fn a_build_on_the_volume_is_used_without_asking_anyone() {
    let dir = tempfile::tempdir().unwrap();
    let first = leak(Service::honest(good_tarball()));
    ensure_build(&first, PLATFORM, &builds(&dir)).await.unwrap();
    // A guest that is offline now still starts its editor.
    let offline = leak(Service::honest(good_tarball()));
    offline.failing.store(100, Ordering::SeqCst);
    let build = ensure_build(&offline, PLATFORM, &builds(&dir))
        .await
        .unwrap();
    assert_eq!(build, builds(&dir).join(COMMIT));
    assert_eq!(offline.requests(), 0);
}

#[tokio::test]
async fn a_download_that_is_not_the_published_file_is_refused_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let wrong = sha(b"something else");
    let service = leak(Service::new(good_tarball(), &wrong));
    let err = ensure_build(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap_err();
    let InstallError::Checksum { expected, actual } = &err else {
        panic!("expected a checksum error, got {err}");
    };
    assert_eq!(expected, &wrong);
    assert_eq!(actual, &sha(&good_tarball()));
    assert!(err.to_string().contains("not the published one"), "{err}");
    assert_eq!(names(&builds(&dir)), Vec::<String>::new());
    assert_eq!(current_build(&builds(&dir)), None);
}

#[tokio::test]
async fn a_published_hash_in_capitals_is_the_same_hash() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = good_tarball();
    let service = leak(Service::new(bytes.clone(), &sha(&bytes).to_uppercase()));
    ensure_build(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap();
}

#[tokio::test]
async fn archives_that_could_write_outside_the_build_or_hold_no_server_are_refused() {
    for (what, entries) in [
        (
            "a path that leaves the folder",
            vec![
                (
                    format!("{TOP}/bin/code-server"),
                    0o755,
                    "#!/bin/sh".to_owned(),
                ),
                (format!("{TOP}/../../escaped"), 0o644, "x".to_owned()),
            ],
        ),
        (
            "an absolute path",
            vec![("/etc/escaped".to_owned(), 0o644, "x".to_owned())],
        ),
        (
            "no server program",
            vec![(format!("{TOP}/product.json"), 0o644, "{}".to_owned())],
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let listed: Vec<(&str, u32, &str)> = entries
            .iter()
            .map(|(p, m, c)| (p.as_str(), *m, c.as_str()))
            .collect();
        let service = leak(Service::honest(tarball(&listed)));
        let err = ensure_build(&service, PLATFORM, &builds(&dir))
            .await
            .unwrap_err();
        assert!(matches!(err, InstallError::Archive(_)), "{what}: {err}");
        assert_eq!(names(&builds(&dir)), Vec::<String>::new(), "{what}");
        assert!(!dir.path().join("escaped").exists(), "{what}");
    }
}

#[tokio::test]
async fn a_link_in_the_archive_is_refused() {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    builder
        .append_link(&mut header, format!("{TOP}/node"), "/usr/bin/node")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let service = leak(Service::honest(
        builder.into_inner().unwrap().finish().unwrap(),
    ));
    let err = ensure_build(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap_err();
    assert!(matches!(err, InstallError::Archive(_)), "{err}");
}

#[tokio::test]
async fn an_answer_about_another_build_or_without_a_usable_hash_is_refused() {
    for (what, versions) in [
        (
            "another build",
            versions_answer(&"0".repeat(40), RELEASE, &sha(b"x")),
        ),
        ("a short hash", versions_answer(COMMIT, RELEASE, "abc")),
        ("not json", "<html>".to_owned()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let service = Service::honest(good_tarball());
        service
            .answers
            .lock()
            .unwrap()
            .insert(versions_url(RELEASE, PLATFORM), versions);
        let service = leak(service);
        let err = install_latest(&service, PLATFORM, &builds(&dir))
            .await
            .unwrap_err();
        assert!(matches!(err, InstallError::Resolve(_)), "{what}: {err}");
        assert_eq!(
            service.downloads.load(Ordering::SeqCst),
            0,
            "{what}: nothing is downloaded unchecked"
        );
    }
    // A latest answer whose commit is not a commit never becomes a path.
    let dir = tempfile::tempdir().unwrap();
    let service = Service::honest(good_tarball());
    service
        .answers
        .lock()
        .unwrap()
        .insert(latest_url(PLATFORM), latest_answer("../../etc", RELEASE));
    let service = leak(service);
    let err = install_latest(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap_err();
    assert!(matches!(err, InstallError::Resolve(_)), "{err}");
}

#[tokio::test]
async fn the_newest_build_already_on_the_volume_is_made_current_without_a_download() {
    let dir = tempfile::tempdir().unwrap();
    let first = leak(Service::honest(good_tarball()));
    ensure_build(&first, PLATFORM, &builds(&dir)).await.unwrap();
    // The pointer is lost (an older build was current, or the file was removed).
    std::fs::remove_file(builds(&dir).join("current")).unwrap();
    let again = leak(Service::honest(good_tarball()));
    let made = install_latest(&again, PLATFORM, &builds(&dir))
        .await
        .unwrap();
    assert_eq!(current_build(&builds(&dir)), Some(made));
    assert_eq!(again.downloads.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn staging_left_by_a_dead_attempt_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let stale = builds(&dir).join(format!("{COMMIT}.partial"));
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("stale"), "x").unwrap();
    let service = leak(Service::honest(good_tarball()));
    let made = ensure_build(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap();
    assert!(!made.join("stale").exists());
    assert!(made.join("bin/code-server").is_file());
}

#[tokio::test]
async fn a_half_made_build_is_not_a_build() {
    let dir = tempfile::tempdir().unwrap();
    let build = builds(&dir).join(COMMIT);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/code-server"), "#!/bin/sh").unwrap();
    std::fs::write(builds(&dir).join("current"), COMMIT).unwrap();
    // No marker: the unpack never finished.
    assert_eq!(current_build(&builds(&dir)), None);
    let service = leak(Service::honest(good_tarball()));
    let made = ensure_build(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap();
    assert_eq!(current_build(&builds(&dir)), Some(made.clone()));
    assert_eq!(
        std::fs::read_to_string(made.join("product.json")).unwrap(),
        "{}",
        "the folder was replaced whole, not mixed with the dead attempt"
    );
}

/// A script that stands in for `curl` or `wget`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn the_downloader_runs_curl_with_https_only_and_wget_when_there_is_no_curl() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    let curl = script(
        dir.path(),
        "curl",
        &format!(
            "printf '%s\\n' \"$@\" >{}\nfor a; do last=$a; done\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output ] && printf body >\"$2\"; shift; done\n[ \"$last\" = https://bad.test/ ] && {{ echo 'curl: (22) 404' >&2; exit 22; }}\necho '{{\"ok\":true}}'",
            log.display()
        ),
    );
    let downloader = Downloader::with_programs(&curl, dir.path().join("no-wget"));
    assert_eq!(
        downloader
            .text("https://update.test/x")
            .await
            .unwrap()
            .trim(),
        r#"{"ok":true}"#
    );
    let args = std::fs::read_to_string(&log).unwrap();
    for wanted in [
        "--fail",
        "--proto\n=https",
        "--proto-redir\n=https",
        "--max-time",
    ] {
        assert!(args.contains(wanted), "{wanted} in {args}");
    }
    assert!(args.trim_end().ends_with("https://update.test/x"), "{args}");

    let target = dir.path().join("download");
    downloader
        .file("https://update.test/file", &target)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "body");
    // A file has no total time limit, only a stall limit.
    let args = std::fs::read_to_string(&log).unwrap();
    assert!(args.contains("--speed-time"), "{args}");
    assert!(!args.contains("--max-time"), "{args}");

    // A failing download says what the tool said.
    let err = downloader.text("https://bad.test/").await.unwrap_err();
    assert!(err.to_string().contains("404"), "{err}");

    // No curl: wget does it, and is told to use HTTPS only.
    let wget = script(
        dir.path(),
        "wget",
        &format!(
            "printf '%s\\n' \"$@\" >{}\necho wgot",
            dir.path().join("wlog").display()
        ),
    );
    let downloader = Downloader::with_programs(dir.path().join("no-curl"), &wget);
    assert_eq!(
        downloader
            .text("https://update.test/y")
            .await
            .unwrap()
            .trim(),
        "wgot"
    );
    let args = std::fs::read_to_string(dir.path().join("wlog")).unwrap();
    assert!(args.contains("--https-only"), "{args}");
    assert!(args.contains("--output-document\n-\n"), "{args}");

    // A curl that cannot be run (not a missing one) is an error of its own, not a fall back.
    let plain_file = dir.path().join("not-executable");
    std::fs::write(&plain_file, "x").unwrap();
    let broken = Downloader::with_programs(&plain_file, &wget);
    let err = broken.text("https://update.test/y").await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");

    // Neither: the message names the missing tools.
    let none = Downloader::with_programs(dir.path().join("no-curl"), dir.path().join("no-wget"));
    let err = none.text("https://update.test/z").await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::NotFound);
    assert!(err.to_string().contains("neither curl nor wget"), "{err}");
}

// The agent's own loop, with a fake server in a real tarball.

struct Fixture {
    _dir: tempfile::TempDir,
    out: PathBuf,
    config: EditorConfig,
}

/// A port nothing listens on now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fixture(telemetry: bool) -> Fixture {
    fixture_updating(telemetry, true)
}

fn fixture_updating(telemetry: bool, auto_update: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut server = EditorServer::microsoft(
        dir.path().join("data").to_string_lossy(),
        dir.path().join("work").to_string_lossy(),
        120,
        telemetry,
        auto_update,
    );
    server.port = free_port();
    let config = EditorConfig {
        server,
        password_file: dir.path().join("run/editor-password"),
    };
    Fixture {
        _dir: dir,
        out,
        config,
    }
}

/// The service whose server records into `out`.
fn recording_service(out: &Path) -> &'static Service {
    leak(Service::honest(server_tarball(out, COMMIT, false)))
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn read(out: &Path, name: &str) -> String {
    std::fs::read_to_string(out.join(name)).unwrap()
}

fn kill(pid: &str) {
    let status = std::process::Command::new("kill")
        .args(["-9", pid.trim()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn the_agent_fetches_the_server_and_starts_it_with_a_token_the_terms_and_no_telemetry() {
    let f = fixture(false);
    let service = recording_service(&f.out);
    let task = tokio::spawn(editor::run_with(f.config.clone(), service));
    until("the first start", || f.out.join("1.pid").exists()).await;

    let server = &f.config.server;
    let args = read(&f.out, "1.args");
    let token_file = f.config.password_file.to_string_lossy();
    for wanted in [
        format!("--host\n127.0.0.1\n--port\n{}\n", server.port),
        format!("--connection-token-file\n{token_file}\n"),
        "--accept-server-license-terms\n".to_owned(),
        "--reconnection-grace-time\n120\n".to_owned(),
        "--disable-telemetry\n".to_owned(),
        format!("--server-data-dir\n{}/server-data\n", server.data),
        format!("--extensions-dir\n{}/extensions\n", server.data),
    ] {
        assert!(args.contains(&wanted), "{wanted:?} in {args}");
    }
    assert!(
        args.trim_end()
            .ends_with(&format!("--default-folder\n{}", server.folder)),
        "{args}"
    );
    assert!(
        !args.contains("serve-web"),
        "the server is started directly, never through serve-web"
    );
    // The token is a long random value in an owner-only file that the server read; the server
    // got no PASSWORD and the arguments never hold the token itself.
    let token = std::fs::read_to_string(&f.config.password_file).unwrap();
    assert_eq!(token.len(), 64);
    assert_eq!(read(&f.out, "1.token"), token);
    assert_eq!(read(&f.out, "1.password"), "");
    assert!(!args.contains(&token));
    let mode = std::fs::metadata(&f.config.password_file)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // The build came from Microsoft once.
    assert_eq!(service.downloads.load(Ordering::SeqCst), 1);

    // A killed server is started again from the build on the volume with the same token: no
    // second download, and a window that was signed in stays signed in.
    kill(&read(&f.out, "1.pid"));
    until("the restart", || f.out.join("2.pid").exists()).await;
    assert_eq!(read(&f.out, "2.token"), token);
    assert_eq!(service.downloads.load(Ordering::SeqCst), 1);
    // Ending the task drops the child, which kills the fake server (`kill_on_drop`).
    task.abort();
}

#[tokio::test]
async fn telemetry_is_left_on_only_when_the_user_ticked_the_box() {
    let f = fixture(true);
    let service = recording_service(&f.out);
    let task = tokio::spawn(editor::run_with(f.config.clone(), service));
    until("the first start", || f.out.join("1.pid").exists()).await;
    let args = read(&f.out, "1.args");
    assert!(!args.contains("--disable-telemetry"), "{args}");
    assert!(args.contains("--accept-server-license-terms"), "{args}");
    task.abort();
}

#[tokio::test]
async fn no_network_is_retried_and_the_server_starts_once_the_update_service_answers() {
    let f = fixture(false);
    let service = recording_service(&f.out);
    service.failing.store(2, Ordering::SeqCst);
    let task = tokio::spawn(editor::run_with(f.config.clone(), service));
    until("the first start", || f.out.join("1.pid").exists()).await;
    // Two failed attempts, then the three requests of a good one.
    assert_eq!(service.requests(), 2 + 3);
    task.abort();
}

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
    until("the first start", || f.out.join("1.pid").exists()).await;
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
        until("the first build to be confirmed", || {
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
    until("the newer build to be staged", || {
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
    until("the newer build to start", || {
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
    until("the previous build to be removed", || {
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
    until("the newer build to be staged", || {
        pointer(&r.builds, "staged").is_some()
    })
    .await;
    assert!(r.control.switch().unwrap().is_some());
    drop(listener);
    // The newer build starts, exits at once, and the first build is started again.
    until("the first build to run again", || {
        r.f.out.join("3.pid").exists()
    })
    .await;
    assert_eq!(read(&r.f.out, "2.build").trim(), NEWER);
    assert_eq!(read(&r.f.out, "3.build").trim(), COMMIT);
    assert_eq!(pointer(&r.builds, "current").as_deref(), Some(COMMIT));
    assert_eq!(pointer(&r.builds, "rejected").as_deref(), Some(NEWER));
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
    let r = running(fixture_updating(false, false), false).await;
    r.publish_newer(false);
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
    until("the newer build to be staged after failures", || {
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
    assert!(!roll_back(&builds).unwrap());
    confirm(&builds).unwrap();
    service.publish(NEWER, NEWER_RELEASE, good_tarball());
    stage_update(&service, PLATFORM, &builds).await.unwrap();
    assert_eq!(switch_to_staged(&builds).unwrap().as_deref(), Some(NEWER));
    assert!(on_trial(&builds));
    // Confirmed: the trial is over and the old build is gone.
    confirm(&builds).unwrap();
    assert!(!on_trial(&builds));
    assert!(!roll_back(&builds).unwrap());
    assert_eq!(names(&builds), [NEWER, "confirmed", "current"]);
    // With nothing current at all, confirming is a no-op.
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    confirm(&empty).unwrap();
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
    confirm(&builds).unwrap();
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
