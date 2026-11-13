// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Read;

fn head(path: &Path, limit: u64) -> String {
    let mut text = String::new();
    let f = std::fs::File::open(path).unwrap();
    f.take(limit).read_to_string(&mut text).unwrap();
    text
}

const TOP: &str = "code-server-9.9.9-linux-amd64";

/// A tarball with `files` (path below the top folder, content, mode) and `links`.
fn tarball(dir: &Path, files: &[(&str, &str, u32)], links: &[&str]) -> PathBuf {
    let path = dir.join("code-server-9.9.9-linux-amd64.tar.gz");
    let gz = GzEncoder::new(std::fs::File::create(&path).unwrap(), Compression::fast());
    let mut b = tar::Builder::new(gz);
    for (name, content, mode) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(*mode);
        h.set_cksum();
        b.append_data(&mut h, format!("{TOP}/{name}"), content.as_bytes())
            .unwrap();
    }
    for link in links {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(EntryType::Symlink);
        h.set_size(0);
        h.set_cksum();
        b.append_link(&mut h, format!("{TOP}/{link}"), "../x/y.js")
            .unwrap();
    }
    b.into_inner().unwrap().finish().unwrap();
    path
}

fn release(version: &str) -> Vec<(&'static str, String, u32)> {
    vec![
        ("bin/code-server", "#!/bin/sh\n".into(), 0o755),
        ("lib/node", "node".into(), 0o755),
        (
            "package.json",
            format!("{{\"version\":\"{version}\"}}"),
            0o644,
        ),
        ("LICENSE", "MIT".into(), 0o644),
        ("ThirdPartyNotices.txt", "notices".into(), 0o644),
    ]
}

fn make(dir: &Path, version: &str, links: &[&str]) -> (DirReleases, Pin) {
    let files = release(version);
    let refs: Vec<(&str, &str, u32)> = files.iter().map(|(n, c, m)| (*n, c.as_str(), *m)).collect();
    let path = tarball(dir, &refs, links);
    let sha = sha256_file(&path).unwrap();
    (
        DirReleases(dir.to_path_buf()),
        Pin {
            version: "9.9.9".into(),
            asset: path.file_name().unwrap().to_string_lossy().into_owned(),
            sha256: sha,
        },
    )
}

#[test]
fn the_pin_list_in_the_repository_parses() {
    let text = std::fs::read_to_string(crate::workspace_root().join(PIN_FILE)).unwrap();
    let pins = parse_pins(&text).unwrap();
    let first = select(&pins, None).unwrap();
    assert!(first.asset.contains(&first.version) && first.asset.ends_with("linux-amd64.tar.gz"));
    assert_eq!(select(&pins, Some(&first.version)).unwrap(), first);
}

#[test]
fn pin_lines_are_checked() {
    let sha = "a".repeat(64);
    for (text, needle) in [
        ("", "no version is pinned"),
        ("# only a comment\n", "no version is pinned"),
        ("4.1.0 asset", "expected"),
        ("4.1.0 asset sum", "not a SHA-256"),
        (&format!("4.x asset {sha}"), "not a dotted number"),
        (&format!("4.1.0 a/b {sha}"), "not a path"),
        (&format!("4.1.0 a {sha} extra"), "expected"),
        (&format!("4.1.0 a {sha}\n4.1.0 b {sha}"), "listed twice"),
    ] {
        let err = parse_pins(text).unwrap_err().to_string();
        assert!(err.contains(needle), "{text:?}: {err}");
    }
    let pins = parse_pins(&format!(
        "# c\n4.2.0 a {sha}\n4.1.0 b {}\n",
        sha.to_uppercase()
    ))
    .unwrap();
    assert_eq!(select(&pins, None).unwrap().version, "4.2.0");
    assert_eq!(select(&pins, Some("4.1.0")).unwrap().sha256, sha);
    let err = select(&pins, Some("3.0.0")).unwrap_err().to_string();
    assert!(
        err.contains("not pinned") && err.contains("4.2.0, 4.1.0"),
        "{err}"
    );
}

#[test]
fn builds_the_folder_with_its_notices_and_leaves_command_shims_out() {
    let dir = tempfile::tempdir().unwrap();
    let (source, pin) = make(dir.path(), "9.9.9", &["node_modules/.bin/semver"]);
    let out = dir.path().join("bundle");
    let licences = dir.path().join("licenses");
    let manifest = assemble(&source, &pin, &out, Some(&licences)).unwrap();
    assert_eq!(manifest.files, 5);
    assert_eq!(manifest.skipped_links, 1);
    assert!(out.join("lib/node").is_file() && out.join("bin/code-server").is_file());
    assert!(!out.join("node_modules/.bin/semver").exists());
    assert!(!out.join(".download").exists());
    assert!(!dir.path().join("bundle.partial").exists());
    let note = std::fs::read_to_string(out.join("PUDDLE-NOTICE.txt")).unwrap();
    assert!(
        note.contains(&pin.sha256) && note.contains("MIT licence"),
        "{note}"
    );
    let json = std::fs::read_to_string(out.join("code-server.json")).unwrap();
    assert!(json.contains("\"version\": \"9.9.9\"") && json.contains(&pin.sha256));
    assert_eq!(head(&licences.join("code-server/LICENSE"), 10), "MIT");
    assert_eq!(
        head(&licences.join("code-server/ThirdPartyNotices.txt"), 20),
        "notices"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| std::fs::metadata(out.join(p)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode("lib/node"), 0o755);
        assert_eq!(mode("LICENSE"), 0o644);
    }
}

#[test]
fn a_changed_download_is_refused_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (source, mut pin) = make(dir.path(), "9.9.9", &[]);
    pin.sha256 = "0".repeat(64);
    let out = dir.path().join("bundle");
    let err = assemble(&source, &pin, &out, None).unwrap_err().to_string();
    assert!(
        err.contains("checksum mismatch") && err.contains(PIN_FILE),
        "{err}"
    );
    assert!(!out.exists() && !dir.path().join("bundle.partial").exists());
}

#[test]
fn a_release_of_another_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (source, pin) = make(dir.path(), "9.9.8", &[]);
    let err = assemble(&source, &pin, &dir.path().join("b"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("9.9.8") && err.contains("9.9.9"), "{err}");
}

#[test]
fn a_missing_file_a_full_output_and_a_stray_link_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = tarball(
        dir.path(),
        &[("package.json", "{\"version\":\"9.9.9\"}", 0o644)],
        &[],
    );
    let pin = Pin {
        version: "9.9.9".into(),
        asset: path.file_name().unwrap().to_string_lossy().into_owned(),
        sha256: sha256_file(&path).unwrap(),
    };
    let source = DirReleases(dir.path().to_path_buf());
    let err = assemble(&source, &pin, &dir.path().join("b"), None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no bin/code-server"), "{err}");

    let full = dir.path().join("full");
    std::fs::create_dir(&full).unwrap();
    std::fs::write(full.join("x"), "x").unwrap();
    assert!(matches!(
        assemble(&source, &pin, &full, None),
        Err(XtaskError::OutputExists(_))
    ));

    let stray = tarball(dir.path(), &[], &["lib/evil-link"]);
    let err = unpack(&stray, &dir.path().join("u"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("lib/evil-link") && err.contains("Symlink"),
        "{err}"
    );
}

#[test]
fn names_that_differ_only_in_case_fail_the_unpack() {
    let dir = tempfile::tempdir().unwrap();
    let path = tarball(
        dir.path(),
        &[("a/Readme", "1", 0o644), ("a/README", "2", 0o644)],
        &[],
    );
    let err = unpack(&path, &dir.path().join("u"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("differ only in case"), "{err}");
    // The same name twice (a later entry replaces an earlier one) is not a collision.
    let path = tarball(dir.path(), &[("a/x", "1", 0o644), ("a/x", "2", 0o644)], &[]);
    assert_eq!(unpack(&path, &dir.path().join("v")).unwrap().files, 2);
}

#[test]
fn a_path_that_leaves_the_folder_is_refused() {
    assert!(relative(Path::new("top/../../x")).is_err());
    assert!(relative(Path::new("/etc/passwd")).is_err());
    assert_eq!(relative(Path::new("top")).unwrap(), None);
    assert_eq!(
        relative(Path::new("top/a/b")).unwrap().as_deref(),
        Some("a/b")
    );
}
