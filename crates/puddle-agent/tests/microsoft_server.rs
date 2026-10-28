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
use puddle_agent::editor::{self, EditorConfig};
use puddle_agent::microsoft::{
    Downloader, Fetch, InstallError, current_build, download_url, ensure_build, install_latest,
    latest_url, versions_url,
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
echo $$ >"OUT/$n.pid"
exec sleep 600
"#;

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
fn latest_answer(commit: &str) -> String {
    format!(
        r#"{{"name":"{RELEASE}","version":"{commit}","productVersion":"{RELEASE}","timestamp":1792690529437}}"#
    )
}

fn versions_answer(commit: &str, hash: &str) -> String {
    format!(
        r#"{{"url":"https://vscode.download.prss.microsoft.com/dbazure/download/stable/{commit}/vscode-server-linux-x64-web.tar.gz","name":"{RELEASE}","version":"{commit}","productVersion":"{RELEASE}","hash":"{commit}","timestamp":1792690529437,"sha256hash":"{hash}","supportsFastUpdate":true}}"#
    )
}

/// A fake update service: answers by URL and counts what it was asked.
struct Service {
    answers: BTreeMap<String, String>,
    tarball: Vec<u8>,
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
            (latest_url(PLATFORM), latest_answer(COMMIT)),
            (
                versions_url(RELEASE, PLATFORM),
                versions_answer(COMMIT, published),
            ),
        ]);
        Self {
            answers,
            tarball,
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
            .get(url)
            .cloned()
            .ok_or_else(|| io::Error::other(format!("404 {url}")))
    }

    async fn file(&self, url: &str, dest: &Path) -> io::Result<()> {
        tokio::task::yield_now().await; // a download is not instant
        self.asked.lock().unwrap().push(url.to_owned());
        self.downloads.fetch_add(1, Ordering::SeqCst);
        self.check_up()?;
        if url != download_url(COMMIT, PLATFORM) {
            return Err(io::Error::other(format!("404 {url}")));
        }
        std::fs::write(dest, &self.tarball)
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
            &FAKE_SERVER.replace("OUT", "@OUT@"),
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
            versions_answer(&"0".repeat(40), &sha(b"x")),
        ),
        ("a short hash", versions_answer(COMMIT, "abc")),
        ("not json", "<html>".to_owned()),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut service = Service::honest(good_tarball());
        service
            .answers
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
    let mut service = Service::honest(good_tarball());
    service
        .answers
        .insert(latest_url(PLATFORM), latest_answer("../../etc"));
    let service = leak(service);
    let err = install_latest(&service, PLATFORM, &builds(&dir))
        .await
        .unwrap_err();
    assert!(matches!(err, InstallError::Resolve(_)), "{err}");
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
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let mut server = EditorServer::microsoft(
        dir.path().join("data").to_string_lossy(),
        dir.path().join("work").to_string_lossy(),
        120,
        telemetry,
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
    let tar = tarball(&[(
        &format!("{TOP}/bin/code-server"),
        0o755,
        &FAKE_SERVER.replace("OUT", &out.to_string_lossy()),
    )]);
    leak(Service::honest(tar))
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
