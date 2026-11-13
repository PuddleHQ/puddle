// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask runtime` against a fixture release: the folder it builds for Windows and for Linux,
//! and every check that must stop it (checksum, version, missing licence entry, existing output,
//! no pinned firmware version).
#![expect(
    clippy::unwrap_used,
    reason = "fixture helpers run outside #[test] but only in tests"
)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use object::write::{Object, StandardSection};
use object::{Architecture, BinaryFormat, Endianness, SectionKind};
use puddle_runtime::{HostOs, RuntimeFiles};
use xtask::checksums::sha256_file;
use xtask::source::{
    CHECKSUMS_ASSET, Commit, Firmware, ForkFacts, LIBKRUNFW_ASSET, LIBKRUNFW_LINUX_ASSET,
    MSB_ASSET, MSB_LINUX_ASSET,
};

const VERSION: &str = "0.7.7-puddle.3";

/// The assembled folders are for either OS, whatever OS runs the test.
const WIN: RuntimeFiles = HostOs::Windows.runtime_files();
const LINUX: RuntimeFiles = HostOs::Linux.runtime_files();

/// The firmware version the fixture's pin list gives `VERSION`'s tag.
const FW_VERSION: &str = "5.6.1";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        Self(
            tempfile::Builder::new()
                .prefix(&format!("xtask-runtime-{tag}-"))
                .tempdir()
                .unwrap()
                .keep(),
        )
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fake_msb(version: &str) -> Vec<u8> {
    fake_msb_in(BinaryFormat::Coff, version)
}

fn fake_msb_in(format: BinaryFormat, version: &str) -> Vec<u8> {
    let mut obj = Object::new(format, Architecture::X86_64, Endianness::Little);
    let text = obj.section_id(StandardSection::Text);
    obj.append_section_data(text, &[0xc3], 1);
    let id = obj.add_section(Vec::new(), b".msbver".to_vec(), SectionKind::ReadOnlyData);
    obj.append_section_data(id, version.as_bytes(), 1);
    obj.write().unwrap()
}

fn tree(crates: &[(&str, &str)]) -> String {
    let lines: Vec<String> = crates.iter().map(|(n, v)| format!("{n} v{v}")).collect();
    format!("root v0.0.0 (/work/root)\n{}\n", lines.join("\n"))
}

fn about(licence: &str, crates: &[(&str, &str)]) -> String {
    let used_by: Vec<_> = crates
        .iter()
        .map(|(n, v)| serde_json::json!({"crate": {"name": n, "version": v, "repository": null}, "path": ""}))
        .collect();
    serde_json::json!({"licenses": [{"name": format!("{licence} licence"), "id": licence, "text": format!("{licence} TEXT with copyright"), "used_by": used_by}]}).to_string()
}

/// A complete fixture release in `<dir>/src`.
fn fixture(dir: &Path, embedded: &str) -> PathBuf {
    let src = dir.join("src");
    for sub in ["fork", "upstream"] {
        std::fs::create_dir_all(src.join(sub)).unwrap();
    }
    let msb = src.join("fork").join(MSB_ASSET);
    std::fs::write(&msb, fake_msb(embedded)).unwrap();
    let msb_linux = src.join("fork").join(MSB_LINUX_ASSET);
    let fw = src.join("fork").join(LIBKRUNFW_ASSET);
    std::fs::write(&fw, b"firmware bytes").unwrap();
    std::fs::write(&msb_linux, fake_msb_in(BinaryFormat::Elf, embedded)).unwrap();
    std::fs::write(
        src.join("fork").join(CHECKSUMS_ASSET),
        format!(
            "{}  {MSB_ASSET}\n{}  {MSB_LINUX_ASSET}\n{}  {LIBKRUNFW_ASSET}\n",
            sha256_file(&msb).unwrap(),
            sha256_file(&msb_linux).unwrap(),
            sha256_file(&fw).unwrap()
        ),
    )
    .unwrap();
    let fw_linux = src.join("upstream").join(LIBKRUNFW_LINUX_ASSET);
    std::fs::write(&fw_linux, b"linux firmware bytes").unwrap();
    std::fs::write(
        src.join("upstream").join(CHECKSUMS_ASSET),
        format!(
            "{}  agentd-x86_64\n{}  {LIBKRUNFW_LINUX_ASSET}\n",
            "0".repeat(64),
            sha256_file(&fw_linux).unwrap()
        ),
    )
    .unwrap();
    write_pins(
        &src,
        &format!("v{VERSION} libkrunfw-linux-version {FW_VERSION}\n"),
    );
    std::fs::write(
        src.join("msb-linux"),
        fake_msb_in(BinaryFormat::Elf, embedded),
    )
    .unwrap();
    let facts = ForkFacts {
        fork_repo: "PuddleHQ/microsandbox".into(),
        upstream_repo: "superradcompany/microsandbox".into(),
        tag: format!("v{VERSION}"),
        upstream_tag: "v0.7.7".into(),
        commits: vec![Commit {
            sha: "c".repeat(40),
            subject: "fix(vsock): tail loss".into(),
        }],
        firmware: Firmware {
            libkrunfw_version: "5.6.1".into(),
            kernel_version: "6.12.109".into(),
            repo_url: "https://github.com/superradcompany/libkrunfw".into(),
            commit: "d".repeat(40),
            fork_tag: None,
        },
    };
    std::fs::write(
        src.join("facts.json"),
        serde_json::to_string(&facts).unwrap(),
    )
    .unwrap();
    let msb_crates = [("tokio", "1.0.0"), ("aws-lc-sys", "0.30.0")];
    std::fs::write(src.join("msb-tree.txt"), tree(&msb_crates)).unwrap();
    std::fs::write(src.join("msb-about.json"), about("MIT", &msb_crates)).unwrap();
    let puddle_crates = [("thiserror", "2.0.0")];
    std::fs::write(src.join("puddle-tree.txt"), tree(&puddle_crates)).unwrap();
    std::fs::write(
        src.join("puddle-about.json"),
        about("Apache-2.0", &puddle_crates),
    )
    .unwrap();
    src
}

/// `<src>/pins.sha256`: the hash of every fixture asset as the release holds it now, under the tag
/// that pins it, and `extra` lines. Call it after the assets are final: a pin made from tampered
/// files is the "list and assets replaced together" case.
fn write_pins(src: &Path, extra: &str) {
    let hash = |dir: &str, asset: &str| sha256_file(&src.join(dir).join(asset)).unwrap();
    let mut pins = format!("# fixture\n{extra}");
    for (tag, dir, asset) in [
        (format!("v{VERSION}"), "fork", MSB_ASSET),
        (format!("v{VERSION}"), "fork", MSB_LINUX_ASSET),
        (format!("v{VERSION}"), "fork", LIBKRUNFW_ASSET),
        ("v0.7.7".to_owned(), "upstream", LIBKRUNFW_LINUX_ASSET),
        (
            "v5.6.1-puddle.1".to_owned(),
            "firmware",
            LIBKRUNFW_LINUX_ASSET,
        ),
    ] {
        if src.join(dir).join(asset).is_file() {
            let _ = writeln!(pins, "{tag} {asset} {}", hash(dir, asset)); // a String can't fail
        }
    }
    std::fs::write(src.join("pins.sha256"), pins).unwrap();
}

fn run(src: &Path, out: &Path) -> xtask::Result<String> {
    run_with(src, out, &["--os", "windows"])
}

fn run_linux(src: &Path, out: &Path) -> xtask::Result<String> {
    run_with(
        src,
        out,
        &[
            "--os",
            "linux",
            "--msb",
            src.join("msb-linux").to_str().unwrap(),
        ],
    )
}

/// Linux with no `--msb`: the msb is the fixture release's `msb-linux-x86_64`.
fn run_linux_download(src: &Path, out: &Path) -> xtask::Result<String> {
    run_with(src, out, &["--os", "linux"])
}

fn run_with(src: &Path, out: &Path, extra: &[&str]) -> xtask::Result<String> {
    let tag = format!("v{VERSION}");
    let pins = src.join("pins.sha256");
    let args = [
        "runtime",
        "--tag",
        &tag,
        "--from",
        src.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--offline",
        "--pins",
        pins.to_str().unwrap(),
    ];
    xtask::run(args.iter().chain(extra).map(Into::into))
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn builds_the_runtime_folder_with_licences() {
    let dir = TempDir::new("ok");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    let msg = run(&src, &out).unwrap();
    assert!(msg.contains(VERSION), "{msg}");

    assert_eq!(
        names(&out),
        [WIN.libkrunfw, "licenses", WIN.msb, "runtime.json"]
    );
    assert_eq!(
        names(&out.join("licenses")),
        [
            "Apache-2.0.txt",
            "GPL-2.0.txt",
            "LGPL-2.1.txt",
            "NOTICE.txt",
            "THIRD-PARTY-msb.txt",
            "THIRD-PARTY-puddle.txt"
        ]
    );

    assert_eq!(std::fs::read(out.join(WIN.msb)).unwrap(), fake_msb(VERSION));
    assert_eq!(
        puddle_runtime::read_embedded_version(&out.join(WIN.msb))
            .unwrap()
            .as_deref(),
        Some(VERSION)
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&out.join("runtime.json"))).unwrap();
    assert_eq!(manifest["version"], VERSION);
    assert_eq!(manifest["fork_tag"], format!("v{VERSION}"));
    assert_eq!(manifest["upstream_tag"], "v0.7.7");
    assert!(manifest.get("libkrunfw_version").is_none());
    assert_eq!(
        manifest["msb_sha256"],
        sha256_file(&out.join(WIN.msb)).unwrap()
    );
    assert_eq!(
        manifest["libkrunfw_sha256"],
        sha256_file(&out.join(WIN.libkrunfw)).unwrap()
    );

    let notice = read(&out.join("licenses/NOTICE.txt"));
    assert!(notice.contains(&format!("{} fix(vsock): tail loss", "c".repeat(40))));
    assert!(notice.contains("Written offer"));
    let msb_notices = read(&out.join("licenses/THIRD-PARTY-msb.txt"));
    assert!(
        msb_notices.contains("aws-lc-sys 0.30.0")
            && msb_notices.contains("MIT TEXT with copyright")
    );
    let puddle_notices = read(&out.join("licenses/THIRD-PARTY-puddle.txt"));
    assert!(puddle_notices.contains("thiserror 2.0.0"));
    assert!(
        !dir.0.join("runtime.partial").exists(),
        "staging folder left behind"
    );
}

fn assert_fails_cleanly(dir: &TempDir, src: &Path, needle: &str) {
    let out = dir.0.join("runtime");
    let err = run(src, &out).unwrap_err().to_string();
    assert!(err.contains(needle), "expected {needle:?} in: {err}");
    assert!(!out.exists(), "a failed build left {}", out.display());
    assert!(
        !dir.0.join("runtime.partial").exists(),
        "staging folder left behind"
    );
}

#[test]
fn a_corrupted_msb_is_refused() {
    let dir = TempDir::new("corrupt");
    let src = fixture(&dir.0, VERSION);
    let mut bytes = std::fs::read(src.join("fork").join(MSB_ASSET)).unwrap();
    bytes.push(0);
    std::fs::write(src.join("fork").join(MSB_ASSET), bytes).unwrap();
    assert_fails_cleanly(&dir, &src, &format!("checksum mismatch for {MSB_ASSET}"));
}

#[test]
fn a_corrupted_firmware_is_refused() {
    let dir = TempDir::new("corruptfw");
    let src = fixture(&dir.0, VERSION);
    std::fs::write(src.join("fork").join(LIBKRUNFW_ASSET), b"tampered").unwrap();
    assert_fails_cleanly(
        &dir,
        &src,
        &format!("checksum mismatch for {LIBKRUNFW_ASSET}"),
    );
}

#[test]
fn an_unlisted_asset_is_refused() {
    let dir = TempDir::new("unlisted");
    let src = fixture(&dir.0, VERSION);
    std::fs::write(src.join("fork").join(CHECKSUMS_ASSET), "").unwrap();
    assert_fails_cleanly(&dir, &src, &format!("{MSB_ASSET} has no entry in"));
}

#[test]
fn an_msb_of_another_version_is_refused_naming_both() {
    let dir = TempDir::new("version");
    let src = fixture(&dir.0, "0.7.7");
    assert_fails_cleanly(
        &dir,
        &src,
        &format!("downloaded msb is version 0.7.7, but the runtime folder is for {VERSION}"),
    );
}

#[test]
fn a_msb_dependency_without_licence_entry_fails() {
    let dir = TempDir::new("msblicence");
    let src = fixture(&dir.0, VERSION);
    std::fs::write(
        src.join("msb-about.json"),
        about("MIT", &[("tokio", "1.0.0")]),
    )
    .unwrap();
    assert_fails_cleanly(
        &dir,
        &src,
        "no licence entry in the msb notices for: aws-lc-sys 0.30.0",
    );
}

#[test]
fn a_puddle_dependency_without_licence_entry_fails() {
    let dir = TempDir::new("puddlelicence");
    let src = fixture(&dir.0, VERSION);
    std::fs::write(src.join("puddle-about.json"), about("MIT", &[])).unwrap();
    assert_fails_cleanly(
        &dir,
        &src,
        "no licence entry in the puddle notices for: thiserror 2.0.0",
    );
}

#[test]
fn an_existing_output_is_never_overwritten() {
    let dir = TempDir::new("exists");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("keep.txt"), "mine").unwrap();
    let err = run(&src, &out).unwrap_err().to_string();
    assert!(err.contains("exists and is not empty"), "{err}");
    assert_eq!(read(&out.join("keep.txt")), "mine");
}

#[test]
fn an_empty_output_folder_is_fine_and_a_stale_staging_folder_is_replaced() {
    let dir = TempDir::new("empty");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(dir.0.join("runtime.partial/old")).unwrap();
    run(&src, &out).unwrap();
    assert!(out.join(WIN.msb).is_file());
    assert!(!out.join("old").exists());
}

#[test]
fn a_missing_part_is_an_io_error() {
    let dir = TempDir::new("missing");
    let src = fixture(&dir.0, VERSION);
    std::fs::remove_file(src.join("facts.json")).unwrap();
    assert_fails_cleanly(&dir, &src, "facts.json");
}

/// Every name in a Linux runtime folder besides the licences and the manifest.
fn linux_files(version: &str) -> Vec<String> {
    let versioned = HostOs::Linux.libkrunfw_versioned(version).unwrap();
    let mut files = vec![
        LINUX.libkrunfw.to_owned(),
        "licenses".to_owned(),
        LINUX.msb.to_owned(),
        "runtime.json".to_owned(),
        versioned,
    ];
    files.sort();
    files
}

#[test]
fn builds_the_linux_runtime_folder_with_both_firmware_names() {
    let dir = TempDir::new("linux");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    let msg = run_linux(&src, &out).unwrap();
    assert!(msg.contains(VERSION) && msg.contains("msb sha256"), "{msg}");

    assert_eq!(names(&out), linux_files(FW_VERSION));
    let firmware = std::fs::read(src.join("upstream").join(LIBKRUNFW_LINUX_ASSET)).unwrap();
    for name in [LINUX.libkrunfw, "libkrunfw.so.5.6.1"] {
        assert_eq!(std::fs::read(out.join(name)).unwrap(), firmware, "{name}");
    }
    assert_eq!(
        std::fs::read(out.join(LINUX.msb)).unwrap(),
        std::fs::read(src.join("msb-linux")).unwrap()
    );
    assert_eq!(
        puddle_runtime::read_embedded_version(&out.join(LINUX.msb))
            .unwrap()
            .as_deref(),
        Some(VERSION)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join(LINUX.msb))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "msb is not executable: {mode:o}");
    }

    let manifest: serde_json::Value =
        serde_json::from_str(&read(&out.join("runtime.json"))).unwrap();
    assert_eq!(manifest["version"], VERSION);
    assert_eq!(manifest["libkrunfw_version"], FW_VERSION);
    assert_eq!(
        manifest["msb_sha256"],
        sha256_file(&out.join(LINUX.msb)).unwrap()
    );
    assert_eq!(
        manifest["libkrunfw_sha256"],
        sha256_file(&out.join("libkrunfw.so.5.6.1")).unwrap()
    );
    assert_eq!(
        manifest["libkrunfw_sha256"],
        sha256_file(&out.join(LINUX.libkrunfw)).unwrap()
    );
    let notice = read(&out.join("licenses/NOTICE.txt"));
    assert!(notice.contains("libkrunfw.so.5 and libkrunfw.so.5.6.1"));
    assert!(!notice.contains(".exe"));
    assert!(!dir.0.join("runtime.partial").exists());
}

#[test]
fn a_pin_bump_changes_the_versioned_firmware_name() {
    let dir = TempDir::new("bump");
    let src = fixture(&dir.0, VERSION);
    write_pins(&src, &format!("v{VERSION} libkrunfw-linux-version 5.7.0\n"));
    let out = dir.0.join("runtime");
    run_linux(&src, &out).unwrap();
    assert_eq!(names(&out), linux_files("5.7.0"));
    assert!(!out.join("libkrunfw.so.5.6.1").exists());
}

#[test]
fn a_linux_folder_needs_a_pinned_firmware_version() {
    let dir = TempDir::new("nopin");
    let src = fixture(&dir.0, VERSION);
    write_pins(&src, "v9.9.9-puddle.1 libkrunfw-linux-version 5.6.1\n");
    let err = run_linux(&src, &dir.0.join("runtime"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("nothing pinned for") && err.contains(VERSION),
        "{err}"
    );
    assert!(!dir.0.join("runtime").exists());
}

#[test]
fn a_linux_folder_needs_the_msb_it_is_given() {
    let dir = TempDir::new("nomsb");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    std::fs::remove_file(src.join("msb-linux")).unwrap();
    let err = run_linux(&src, &out).unwrap_err().to_string();
    assert!(err.contains("msb-linux"), "{err}");
    assert!(!out.exists());
}

#[test]
fn a_linux_folder_downloads_the_releases_msb_and_verifies_it() {
    let dir = TempDir::new("linuxdownload");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    run_linux_download(&src, &out).unwrap();
    assert_eq!(names(&out), linux_files(FW_VERSION));
    assert_eq!(
        std::fs::read(out.join(LINUX.msb)).unwrap(),
        std::fs::read(src.join("fork").join(MSB_LINUX_ASSET)).unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join(LINUX.msb))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "msb is not executable: {mode:o}");
    }

    // A release without the asset, or with different bytes than its checksum list, stops it.
    std::fs::write(src.join("fork").join(MSB_LINUX_ASSET), b"tampered").unwrap();
    let out = dir.0.join("tampered");
    let err = run_linux_download(&src, &out).unwrap_err().to_string();
    assert!(
        err.contains(&format!("checksum mismatch for {MSB_LINUX_ASSET}")),
        "{err}"
    );
    std::fs::remove_file(src.join("fork").join(MSB_LINUX_ASSET)).unwrap();
    let out = dir.0.join("missing");
    let err = run_linux_download(&src, &out).unwrap_err().to_string();
    assert!(err.contains(MSB_LINUX_ASSET), "{err}");
    assert!(!out.exists() && !dir.0.join("missing.partial").exists());
}

#[test]
fn a_linux_msb_of_another_version_is_refused() {
    let dir = TempDir::new("linuxversion");
    let src = fixture(&dir.0, "0.7.7");
    let out = dir.0.join("runtime");
    let err = run_linux(&src, &out).unwrap_err().to_string();
    assert!(
        err.contains(&format!(
            "downloaded msb is version 0.7.7, but the runtime folder is for {VERSION}"
        )),
        "{err}"
    );
    assert!(!out.exists() && !dir.0.join("runtime.partial").exists());
}

#[test]
fn a_corrupted_linux_firmware_is_refused() {
    let dir = TempDir::new("linuxfw");
    let src = fixture(&dir.0, VERSION);
    std::fs::write(
        src.join("upstream").join(LIBKRUNFW_LINUX_ASSET),
        b"tampered",
    )
    .unwrap();
    let out = dir.0.join("runtime");
    let err = run_linux(&src, &out).unwrap_err().to_string();
    assert!(
        err.contains(&format!("checksum mismatch for {LIBKRUNFW_LINUX_ASSET}")),
        "{err}"
    );
    assert!(!out.exists() && !dir.0.join("runtime.partial").exists());
}

#[test]
fn the_options_of_the_other_os_are_refused() {
    let dir = TempDir::new("options");
    let src = fixture(&dir.0, VERSION);
    let out = dir.0.join("runtime");
    let err = run_with(&src, &out, &["--os", "windows", "--msb", "x"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("--msb is for Linux"), "{err}");
    let err = run_with(&src, &out, &["--os", "macos"])
        .unwrap_err()
        .to_string();
    assert!(err.contains("windows and linux only"), "{err}");
    assert!(!out.exists());
}

/// The fixture with a release of the libkrunfw fork pinned for the tag: its own firmware, checksum
/// list and firmware facts. Returns the fork firmware's bytes.
fn with_fork_firmware(src: &Path) -> Vec<u8> {
    let bytes = b"fork firmware bytes".to_vec();
    let dir = src.join("firmware");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(LIBKRUNFW_LINUX_ASSET), &bytes).unwrap();
    std::fs::write(
        dir.join(CHECKSUMS_ASSET),
        format!(
            "{}  {LIBKRUNFW_LINUX_ASSET}\n{}  built-config-x86_64\n",
            sha256_file(&dir.join(LIBKRUNFW_LINUX_ASSET)).unwrap(),
            "0".repeat(64)
        ),
    )
    .unwrap();
    write_pins(
        src,
        &format!(
            "v{VERSION} libkrunfw-linux-version {FW_VERSION}\nv{VERSION} libkrunfw-linux-tag v5.6.1-puddle.1\n"
        ),
    );
    let mut facts: ForkFacts = serde_json::from_str(&read(&src.join("facts.json"))).unwrap();
    facts.firmware = Firmware {
        libkrunfw_version: FW_VERSION.into(),
        kernel_version: "6.12.113".into(),
        repo_url: "https://github.com/PuddleHQ/libkrunfw".into(),
        commit: "e".repeat(40),
        fork_tag: Some("v5.6.1-puddle.1".into()),
    };
    std::fs::write(
        src.join("facts.json"),
        serde_json::to_string(&facts).unwrap(),
    )
    .unwrap();
    bytes
}

#[test]
fn a_pinned_libkrunfw_fork_release_is_the_linux_firmware_and_the_notice_says_so() {
    let dir = TempDir::new("forkfw");
    let src = fixture(&dir.0, VERSION);
    let firmware = with_fork_firmware(&src);
    let out = dir.0.join("runtime");
    run_linux(&src, &out).unwrap();
    assert_eq!(names(&out), linux_files(FW_VERSION));
    for name in [LINUX.libkrunfw, "libkrunfw.so.5.6.1"] {
        assert_eq!(std::fs::read(out.join(name)).unwrap(), firmware, "{name}");
    }
    let manifest: serde_json::Value =
        serde_json::from_str(&read(&out.join("runtime.json"))).unwrap();
    assert_eq!(manifest["libkrunfw_fork_tag"], "v5.6.1-puddle.1");
    assert_eq!(
        manifest["libkrunfw_sha256"],
        sha256_file(&out.join(LINUX.libkrunfw)).unwrap()
    );
    let notice = read(&out.join("licenses/NOTICE.txt"));
    assert!(
        notice.contains(
            "built by Puddle from https://github.com/PuddleHQ/libkrunfw at tag v5.6.1-puddle.1"
        ) && notice.contains("no /dev/mem"),
        "{notice}"
    );
    assert!(!notice.contains("unchanged"), "{notice}");
    assert!(notice.contains(&format!(
        "https://github.com/PuddleHQ/libkrunfw/tree/{}",
        "e".repeat(40)
    )));
}

#[test]
fn a_corrupted_libkrunfw_fork_firmware_is_refused() {
    let dir = TempDir::new("forkbad");
    let src = fixture(&dir.0, VERSION);
    with_fork_firmware(&src);
    std::fs::write(
        src.join("firmware").join(LIBKRUNFW_LINUX_ASSET),
        b"tampered",
    )
    .unwrap();
    let out = dir.0.join("runtime");
    let err = run_linux(&src, &out).unwrap_err().to_string();
    assert!(
        err.contains(&format!("checksum mismatch for {LIBKRUNFW_LINUX_ASSET}"))
            && err.contains("PuddleHQ/libkrunfw v5.6.1-puddle.1"),
        "{err}"
    );
    assert!(!out.exists() && !dir.0.join("runtime.partial").exists());
}

#[test]
fn a_pinned_fork_release_the_facts_do_not_describe_is_refused() {
    let dir = TempDir::new("forkfacts");
    let src = fixture(&dir.0, VERSION);
    with_fork_firmware(&src);
    // Back to facts about upstream's firmware, while the pin still names the fork release.
    let fresh = fixture(&dir.0.join("fresh"), VERSION);
    std::fs::copy(fresh.join("facts.json"), src.join("facts.json")).unwrap();
    let out = dir.0.join("runtime");
    let err = run_linux(&src, &out).unwrap_err().to_string();
    assert!(err.contains("v5.6.1-puddle.1"), "{err}");
    assert!(!out.exists());
}

#[test]
fn the_windows_firmware_is_the_fork_releases_dll_and_the_notice_says_so() {
    let dir = TempDir::new("winfw");
    let src = fixture(&dir.0, VERSION);
    // A different dll under upstream's release must never be the one shipped.
    std::fs::write(src.join("upstream").join(LIBKRUNFW_ASSET), b"upstream dll").unwrap();
    let out = dir.0.join("out");
    run(&src, &out).unwrap();
    assert_eq!(
        std::fs::read(out.join(WIN.libkrunfw)).unwrap(),
        b"firmware bytes"
    );
    let notice = read(&out.join("licenses/NOTICE.txt"));
    assert!(notice.contains(&format!("the release v{VERSION} of PuddleHQ/microsandbox")));
    assert!(!notice.contains("unchanged. The"));
}

/// `cargo xtask runtime` and the CI fetch must ship and run the same dll: both take it from the
/// fork release named in the pin list, and the list pins it for the version the build expects.
#[test]
fn the_windows_dll_xtask_ships_is_the_one_the_ci_fetch_pins() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = read(&root.join("ci/fetch-msb.sh"));
    let fetches_from_fork = script.lines().any(|l| {
        l.trim_start().starts_with("fetch \"$fork_repo\" \"$tag\"")
            && l.contains(&LIBKRUNFW_ASSET.replace("x86_64", "$arch"))
    });
    assert!(
        fetches_from_fork,
        "ci/fetch-msb.sh no longer fetches {LIBKRUNFW_ASSET} from the fork release"
    );
    let tag = format!("v{}", puddle_runtime::BUILT_FOR);
    let pins = read(&root.join("ci/msb-runtime.sha256"));
    assert!(
        pins.lines().any(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            f.len() == 3 && f[0] == tag && f[1] == LIBKRUNFW_ASSET
        }),
        "ci/msb-runtime.sha256 pins no {LIBKRUNFW_ASSET} for {tag}"
    );
}

/// Replaces `asset` under `<src>/<dir>` with `bytes` and rewrites that release's checksum list to
/// match, as an attacker who controls the release would; the pin list is not touched.
fn swap_in_release(src: &Path, dir: &str, asset: &str, bytes: &[u8]) {
    let dir = src.join(dir);
    std::fs::write(dir.join(asset), bytes).unwrap();
    let list = read(&dir.join(CHECKSUMS_ASSET));
    let new = list
        .lines()
        .map(|line| match line.split_once("  ") {
            Some((_, name)) if name == asset => {
                format!("{}  {asset}", sha256_file(&dir.join(asset)).unwrap())
            }
            _ => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(dir.join(CHECKSUMS_ASSET), new + "\n").unwrap();
}

/// Asserts the build stopped on the pin list, naming the file, both hashes and the pin file.
fn assert_pin_mismatch(err: &str, asset: &str, pinned: &str, got: &str) {
    assert!(
        err.contains(&format!("checksum mismatch for {asset}"))
            && err.contains("ci/msb-runtime.sha256")
            && err.contains(pinned)
            && err.contains(got),
        "{err}"
    );
}

fn pin_of(src: &Path, tag: &str, asset: &str) -> String {
    let pins = read(&src.join("pins.sha256"));
    xtask::pin::pinned(&pins, tag, asset).unwrap().to_owned()
}

#[test]
fn an_msb_the_release_list_vouches_for_but_the_pin_list_does_not_is_refused() {
    for (name, linux) in [("pinmsbwin", false), ("pinmsblinux", true)] {
        let dir = TempDir::new(name);
        let src = fixture(&dir.0, VERSION);
        let (asset, os) = if linux {
            (MSB_LINUX_ASSET, "linux")
        } else {
            (MSB_ASSET, "windows")
        };
        let pinned = pin_of(&src, &format!("v{VERSION}"), asset);
        // Same embedded version, other bytes, and a release list that agrees with them.
        let mut bytes = std::fs::read(src.join("fork").join(asset)).unwrap();
        bytes.push(0);
        swap_in_release(&src, "fork", asset, &bytes);
        let got = sha256_file(&src.join("fork").join(asset)).unwrap();
        let out = dir.0.join("runtime");
        let err = run_with(&src, &out, &["--os", os]).unwrap_err().to_string();
        assert_pin_mismatch(&err, asset, &pinned, &got);
        assert!(!out.exists() && !dir.0.join("runtime.partial").exists());
    }
}

#[test]
fn a_firmware_the_release_list_vouches_for_but_the_pin_list_does_not_is_refused() {
    let dir = TempDir::new("pinfw");
    let src = fixture(&dir.0, VERSION);
    let cases: [(&str, &str, &str, &str); 2] = [
        ("windows", "fork", LIBKRUNFW_ASSET, "v0.7.7-puddle.3"),
        ("linux", "upstream", LIBKRUNFW_LINUX_ASSET, "v0.7.7"),
    ];
    for (os, release, asset, tag) in cases {
        let pinned = pin_of(&src, tag, asset);
        swap_in_release(&src, release, asset, b"swapped firmware");
        let got = sha256_file(&src.join(release).join(asset)).unwrap();
        let out = dir.0.join(format!("runtime-{os}"));
        let err = run_with(&src, &out, &["--os", os]).unwrap_err().to_string();
        assert_pin_mismatch(&err, asset, &pinned, &got);
        assert!(!out.exists());
    }
}

#[test]
fn a_libkrunfw_fork_firmware_the_pin_list_does_not_vouch_for_is_refused() {
    let dir = TempDir::new("pinforkfw");
    let src = fixture(&dir.0, VERSION);
    with_fork_firmware(&src);
    let pinned = pin_of(&src, "v5.6.1-puddle.1", LIBKRUNFW_LINUX_ASSET);
    swap_in_release(
        &src,
        "firmware",
        LIBKRUNFW_LINUX_ASSET,
        b"swapped fork firmware",
    );
    let got = sha256_file(&src.join("firmware").join(LIBKRUNFW_LINUX_ASSET)).unwrap();
    let err = run_linux(&src, &dir.0.join("runtime"))
        .unwrap_err()
        .to_string();
    assert_pin_mismatch(&err, LIBKRUNFW_LINUX_ASSET, &pinned, &got);
}

#[test]
fn a_download_with_no_line_in_the_pin_list_is_refused_naming_it() {
    for (os, asset) in [("windows", MSB_ASSET), ("linux", MSB_LINUX_ASSET)] {
        let dir = TempDir::new("pinmissing");
        let src = fixture(&dir.0, VERSION);
        let pins = read(&src.join("pins.sha256"));
        let kept: Vec<_> = pins.lines().filter(|l| !l.contains(asset)).collect();
        std::fs::write(src.join("pins.sha256"), kept.join("\n") + "\n").unwrap();
        let out = dir.0.join("runtime");
        let err = run_with(&src, &out, &["--os", os]).unwrap_err().to_string();
        assert!(
            err.contains(&format!("{asset} has no entry in ci/msb-runtime.sha256")),
            "{err}"
        );
        assert!(!out.exists());
    }
}
