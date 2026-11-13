// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest as _, Sha256};

use super::*;

const BINARY_BYTES: &[u8] = b"#!/bin/sh\necho mise 9.9.9\n";

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// A tarball the way mise packs it: a top folder with the binary, a manual page and a licence.
fn tarball_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for (path, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(EntryType::Regular);
        header.set_cksum();
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn good_tarball() -> Vec<u8> {
    tarball_with(&[
        ("mise/man/man1/mise.1", b".TH mise"),
        ("mise/bin/mise", BINARY_BYTES),
        ("mise/LICENSE", b"MIT"),
    ])
}

fn pin_for(tarball: &[u8]) -> Pin {
    Pin {
        version: "9.9.9".to_owned(),
        asset: "mise-v9.9.9-linux-x64-musl.tar.gz".to_owned(),
        sha256: hex(tarball),
    }
}

/// A [`Fetch`] that serves one body (or fails) and counts its downloads.
struct Server {
    body: Mutex<io::Result<Vec<u8>>>,
    asked: Mutex<Vec<String>>,
    downloads: AtomicUsize,
}

impl Server {
    fn serving(body: Vec<u8>) -> Self {
        Self::with(Ok(body))
    }
    fn with(body: io::Result<Vec<u8>>) -> Self {
        Self {
            body: Mutex::new(body),
            asked: Mutex::new(Vec::new()),
            downloads: AtomicUsize::new(0),
        }
    }
}

impl Fetch for Server {
    async fn text(&self, _url: &str) -> io::Result<String> {
        tokio::task::yield_now().await; // a network answer is not instant
        Err(io::Error::other("mise never asks for text"))
    }
    async fn file(&self, url: &str, dest: &Path) -> io::Result<()> {
        tokio::task::yield_now().await;
        self.downloads.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().push(url.to_owned());
        match &*self.body.lock().unwrap() {
            Ok(bytes) => std::fs::write(dest, bytes),
            Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
        }
    }
}

fn leftovers(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir.join("bin"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.extend(
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.')),
    );
    names.sort();
    names
}

#[tokio::test]
async fn a_good_download_is_installed_and_nothing_else_stays() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = good_tarball();
    let pin = pin_for(&tarball);
    let server = Server::serving(tarball);
    let outcome = fetch(&server, &pin, dir.path()).await.unwrap();
    assert_eq!(outcome, Outcome::Installed);
    assert_eq!(
        server.asked.lock().unwrap().as_slice(),
        ["https://mise.jdx.dev/v9.9.9/mise-v9.9.9-linux-x64-musl.tar.gz"]
    );
    assert_eq!(
        std::fs::read(dir.path().join(BINARY)).unwrap(),
        BINARY_BYTES
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(BINARY))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join(MARKER)).unwrap(),
        format!("9.9.9 {} {}\n", pin.sha256, hex(BINARY_BYTES))
    );
    // Only the binary and the marker: no tarball, no partial file, nothing of the rest of the tarball.
    assert_eq!(leftovers(dir.path()), [".puddle-mise", "mise"]);
}

#[tokio::test]
async fn a_second_run_downloads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = good_tarball();
    let pin = pin_for(&tarball);
    let server = Server::serving(tarball);
    fetch(&server, &pin, dir.path()).await.unwrap();
    assert_eq!(
        fetch(&server, &pin, dir.path()).await.unwrap(),
        Outcome::Current
    );
    assert_eq!(server.downloads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_changed_binary_is_downloaded_again() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = good_tarball();
    let pin = pin_for(&tarball);
    let server = Server::serving(tarball);
    fetch(&server, &pin, dir.path()).await.unwrap();
    // A `mise self-update` in a terminal, or a damaged file.
    std::fs::write(dir.path().join(BINARY), b"something else").unwrap();
    assert_eq!(
        fetch(&server, &pin, dir.path()).await.unwrap(),
        Outcome::Installed
    );
    assert_eq!(
        std::fs::read(dir.path().join(BINARY)).unwrap(),
        BINARY_BYTES
    );
    assert_eq!(server.downloads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_new_pin_replaces_the_old_binary() {
    let dir = tempfile::tempdir().unwrap();
    let old = good_tarball();
    fetch(&Server::serving(old.clone()), &pin_for(&old), dir.path())
        .await
        .unwrap();
    let newer = tarball_with(&[("mise/bin/mise", b"newer")]);
    let mut pin = pin_for(&newer);
    pin.version = "9.9.10".to_owned();
    assert_eq!(
        fetch(&Server::serving(newer), &pin, dir.path())
            .await
            .unwrap(),
        Outcome::Installed
    );
    assert_eq!(std::fs::read(dir.path().join(BINARY)).unwrap(), b"newer");
}

#[tokio::test]
async fn a_download_with_the_wrong_hash_is_refused_and_leaves_no_binary() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin_for(&good_tarball());
    let server = Server::serving(tarball_with(&[("mise/bin/mise", b"tampered")]));
    let err = fetch(&server, &pin, dir.path()).await.unwrap_err();
    assert!(matches!(err, MiseError::Checksum { .. }), "{err}");
    assert!(err.is_refusal());
    assert_eq!(
        err.to_string(),
        "the mise download does not match the checksum Puddle expects"
    );
    assert!(!dir.path().join(BINARY).exists());
    assert!(!dir.path().join(MARKER).exists());
    assert_eq!(leftovers(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_cut_short_download_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let tarball = good_tarball();
    let pin = pin_for(&tarball);
    let cut: Vec<u8> = tarball.iter().copied().take(tarball.len() / 2).collect();
    let err = fetch(&Server::serving(cut), &pin, dir.path())
        .await
        .unwrap_err();
    assert!(matches!(err, MiseError::Checksum { .. }), "{err}");
    assert!(!dir.path().join(BINARY).exists());
}

#[tokio::test]
async fn an_archive_without_the_binary_at_its_place_is_refused() {
    for entries in [
        vec![("mise/LICENSE", &b"MIT"[..])],
        vec![("elsewhere/bin/mise", &b"x"[..])],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let tarball = tarball_with(&entries);
        let pin = pin_for(&tarball);
        let err = fetch(&Server::serving(tarball), &pin, dir.path())
            .await
            .unwrap_err();
        assert!(matches!(err, MiseError::Archive(_)), "{entries:?}: {err}");
        assert!(err.is_refusal());
        assert!(!dir.path().join(BINARY).exists(), "{entries:?}");
        assert!(!dir.path().join(MARKER).exists(), "{entries:?}");
    }
}

#[tokio::test]
async fn a_link_where_the_binary_belongs_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(EntryType::Symlink);
    header.set_cksum();
    builder
        .append_link(&mut header, "mise/bin/mise", "/etc/passwd")
        .unwrap();
    let tarball = builder.into_inner().unwrap().finish().unwrap();
    let pin = pin_for(&tarball);
    let err = fetch(&Server::serving(tarball), &pin, dir.path())
        .await
        .unwrap_err();
    assert!(matches!(err, MiseError::Archive(_)), "{err}");
    assert!(!dir.path().join(BINARY).exists());
}

#[tokio::test]
async fn a_garbled_archive_with_the_right_hash_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let junk = b"this is not a tarball at all".to_vec();
    let pin = pin_for(&junk);
    let err = fetch(&Server::serving(junk), &pin, dir.path())
        .await
        .unwrap_err();
    assert!(matches!(err, MiseError::Archive(_)), "{err}");
}

#[tokio::test]
async fn a_network_failure_is_a_download_error_that_is_not_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin_for(&good_tarball());
    let server = Server::with(Err(io::Error::other("curl failed (exit status: 56): 403")));
    let err = fetch(&server, &pin, dir.path()).await.unwrap_err();
    assert!(matches!(err, MiseError::Download(_)), "{err}");
    assert!(!err.is_refusal());
    assert!(
        err.to_string()
            .starts_with("Puddle could not download mise: "),
        "{err}"
    );
    assert!(err.to_string().contains("403"), "{err}");
    assert!(!dir.path().join(BINARY).exists());
}

#[tokio::test]
async fn no_downloader_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let pin = pin_for(&good_tarball());
    let server = Server::with(Err(io::Error::new(
        io::ErrorKind::NotFound,
        "neither curl nor wget",
    )));
    let err = fetch(&server, &pin, dir.path()).await.unwrap_err();
    assert!(matches!(err, MiseError::NoDownloader), "{err}");
    assert_eq!(
        err.to_string(),
        "the image has neither curl nor wget, which Puddle needs to download mise"
    );
}

#[tokio::test]
async fn an_unwritable_folder_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"").unwrap();
    let tarball = good_tarball();
    let err = fetch(
        &Server::serving(tarball.clone()),
        &pin_for(&tarball),
        &file.join("mise"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, MiseError::Io(_)), "{err}");
}

#[test]
fn the_real_pin_file_has_a_line_for_each_guest_architecture() {
    let x64 = Pin::for_arch(PINS, "x86_64").unwrap();
    let arm = Pin::for_arch(PINS, "aarch64").unwrap();
    assert_eq!(x64.version, arm.version);
    assert_eq!(
        x64.asset,
        format!("mise-v{}-linux-x64-musl.tar.gz", x64.version)
    );
    assert_eq!(
        arm.asset,
        format!("mise-v{}-linux-arm64-musl.tar.gz", arm.version)
    );
    assert_ne!(x64.sha256, arm.sha256);
    assert_eq!(
        x64.url(),
        format!("https://mise.jdx.dev/v{}/{}", x64.version, x64.asset)
    );
}

#[test]
fn an_architecture_without_a_pin_is_named() {
    for arch in ["riscv64", "x86", "arm"] {
        let err = Pin::for_arch(PINS, arch).unwrap_err();
        assert!(matches!(err, MiseError::Unsupported(_)));
        assert_eq!(
            err.to_string(),
            format!("mise has no build for the {arch} architecture pinned in this Puddle")
        );
    }
    // A file that pins only one architecture.
    let one = format!(
        "1.0.0 mise-v1.0.0-linux-x64-musl.tar.gz {}\n",
        "a".repeat(64)
    );
    assert!(Pin::for_arch(&one, "x86_64").is_ok());
    assert!(matches!(
        Pin::for_arch(&one, "aarch64"),
        Err(MiseError::Unsupported(_))
    ));
}

#[test]
fn a_damaged_pin_file_is_not_used() {
    let asset = "mise-v1.0.0-linux-x64-musl.tar.gz";
    for text in [
        String::new(),
        "# only a comment\n".to_owned(),
        format!("1.0.0 {asset} abc\n"),
        format!("1.0.0 {asset} {}\n", "G".repeat(64)),
        format!("1.0.0 {asset} {} extra\n", "a".repeat(64)),
    ] {
        let err = Pin::for_arch(&text, "x86_64").unwrap_err();
        assert!(matches!(err, MiseError::Pins(_)), "{text:?}: {err}");
    }
}

#[test]
fn only_the_first_version_is_used() {
    let text = format!(
        "# c\n2.0.0 mise-v2.0.0-linux-x64-musl.tar.gz {}\n1.0.0 mise-v1.0.0-linux-x64-musl.tar.gz {}\n",
        "b".repeat(64),
        "a".repeat(64)
    );
    assert_eq!(Pin::for_arch(&text, "x86_64").unwrap().version, "2.0.0");
}

#[test]
fn the_request_is_one_folder() {
    assert_eq!(
        Request::parse(&["/v/.puddle/mise"]).unwrap().dir,
        PathBuf::from("/v/.puddle/mise")
    );
    for bad in [&[][..], &[""], &["a", "b"]] {
        assert_eq!(Request::parse(bad).unwrap_err(), USAGE);
    }
}

#[test]
fn outcomes_have_the_words_the_host_reads() {
    assert_eq!(Outcome::Current.as_str(), "current");
    assert_eq!(Outcome::Installed.as_str(), "installed");
}
