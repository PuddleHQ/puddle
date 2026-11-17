// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::Write as _;
use std::sync::Arc;

use puddle_store::{Limits, Store, SystemClock};

use super::*;
use crate::HostPaths;
use crate::backup::{BACKUP_DATABASE, BACKUP_MANIFEST, BACKUP_SETTINGS, BACKUP_WORKSPACES};

/// A real backup of a fresh data folder, as one archive.
fn real_archive(root: &Path) -> Vec<u8> {
    let paths = HostPaths::new(root.join("data"));
    fs::create_dir_all(paths.settings()).unwrap();
    fs::write(paths.settings().join("global.json"), br#"{"version":1}"#).unwrap();
    fs::write(paths.workspace_book(), br#"{"version":4,"workspaces":[]}"#).unwrap();
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    let mut archive = crate::backup_archive(&paths).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    bytes
}

/// One tar entry, written byte for byte so a hostile name (`..`, absolute) survives.
struct Entry<'a> {
    name: &'a str,
    kind: tar::EntryType,
    body: &'a [u8],
    link: Option<&'a str>,
}

fn file<'a>(name: &'a str, body: &'a [u8]) -> Entry<'a> {
    Entry {
        name,
        kind: tar::EntryType::Regular,
        body,
        link: None,
    }
}

fn gz_tar(entries: &[Entry<'_>]) -> Vec<u8> {
    let mut raw = Vec::new();
    for e in entries {
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..e.name.len()].copy_from_slice(e.name.as_bytes());
        header.set_entry_type(e.kind);
        header.set_size(e.body.len() as u64);
        header.set_mode(0o600);
        if let Some(link) = e.link {
            header.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        header.set_cksum();
        raw.extend_from_slice(header.as_bytes());
        raw.extend_from_slice(e.body);
        raw.resize(raw.len().next_multiple_of(512), 0);
    }
    raw.extend_from_slice(&[0; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    gz.finish().unwrap()
}

fn good_db(root: &Path) -> Vec<u8> {
    let path = root.join("good.db");
    drop(Store::open(&path, Arc::new(SystemClock), Limits::default()).unwrap());
    // One file: WAL content folded in.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE;")
        .unwrap();
    fs::read(&path).unwrap()
}

fn refusal(result: Result<Staged, RestoreError>) -> Refusal {
    match result {
        Err(RestoreError::Refused(r)) => r,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn stage(bytes: &[u8], root: &Path) -> Result<Staged, RestoreError> {
    stage_archive(bytes, &root.join("staging"))
}

fn nothing_staged(root: &Path) {
    let staging = root.join("staging");
    let left = fs::read_dir(&staging).map_or(0, Iterator::count);
    assert_eq!(left, 0, "a refused file leaves no staging folder");
}

#[test]
fn a_real_backup_is_staged_with_its_manifest() {
    let root = tempfile::tempdir().unwrap();
    let archive = real_archive(root.path());
    let staged = stage(&archive, root.path()).unwrap();
    assert!(staged.dir.join(BACKUP_DATABASE).is_file());
    assert!(staged.dir.join(BACKUP_WORKSPACES).is_file());
    assert!(
        staged
            .dir
            .join(BACKUP_SETTINGS)
            .join("global.json")
            .is_file()
    );
    let manifest = staged.manifest.unwrap();
    assert_eq!(manifest.format, 1);
    assert_eq!(staged.schema_version, puddle_store::SCHEMA_VERSION);
}

#[test]
fn a_backup_without_a_manifest_is_format_one() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let archive = gz_tar(&[file("puddle.db", &db)]);
    let staged = stage(&archive, root.path()).unwrap();
    assert!(staged.manifest.is_none());
}

#[test]
fn a_file_that_is_not_gzip_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let r = refusal(stage(b"just some text, not an archive at all", root.path()));
    assert_eq!(r, Refusal::NotGzip);
    assert!(r.to_string().contains("not a gzip"), "{r}");
    nothing_staged(root.path());
}

#[test]
fn a_gzip_file_that_is_not_a_tar_archive_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&[b'x'; 2000]).unwrap();
    let r = refusal(stage(&gz.finish().unwrap(), root.path()));
    assert!(matches!(r, Refusal::Damaged(_)), "{r}");
    nothing_staged(root.path());
}

#[test]
fn an_entry_outside_the_three_names_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let archive = gz_tar(&[file("puddle.db", &db), file("login-markers/a", b"x")]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::UnknownEntry("login-markers/a".into()));
    nothing_staged(root.path());
}

#[test]
fn a_path_above_the_top_level_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[file("../escape", b"x")]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::UnsafePath("../escape".into()));
    assert!(!root.path().join("escape").exists());
    nothing_staged(root.path());
}

#[test]
fn a_path_that_climbs_out_from_below_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[file("settings/../../escape", b"x")]);
    let r = refusal(stage(&archive, root.path()));
    assert!(matches!(r, Refusal::UnsafePath(_)), "{r}");
    nothing_staged(root.path());
}

#[test]
fn an_absolute_path_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[file("/etc/escape", b"x")]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::UnsafePath("/etc/escape".into()));
    nothing_staged(root.path());
}

#[test]
fn a_symbolic_link_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[Entry {
        name: "settings",
        kind: tar::EntryType::Symlink,
        body: b"",
        link: Some("/etc"),
    }]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::Link("settings".into()));
    nothing_staged(root.path());
}

#[test]
fn a_hard_link_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[Entry {
        name: "workspaces.json",
        kind: tar::EntryType::Link,
        body: b"",
        link: Some("puddle.db"),
    }]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::Link("workspaces.json".into()));
}

#[test]
fn an_unpacked_size_over_the_limit_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let archive = gz_tar(&[
        file("puddle.db", &db),
        file("workspaces.json", &[b' '; 4096]),
    ]);
    let r = refusal(stage_archive_limited(
        &archive[..],
        &root.path().join("staging"),
        db.len() as u64 + 100,
    ));
    assert!(r == Refusal::TooLarge, "{r}");
    assert!(r.to_string().contains("4 GiB"), "{r}");
    nothing_staged(root.path());
}

#[test]
fn the_limit_is_four_gibibytes() {
    assert_eq!(MAX_UNPACKED_BYTES, 4 * 1024 * 1024 * 1024);
}

#[test]
fn an_archive_without_a_database_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[file("workspaces.json", b"{}")]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::NoDatabase);
    nothing_staged(root.path());
}

#[test]
fn a_database_that_fails_the_integrity_check_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[file("puddle.db", &[b'x'; 8192])]);
    let r = refusal(stage(&archive, root.path()));
    assert!(matches!(r, Refusal::Database(_)), "{r}");
    nothing_staged(root.path());
}

#[test]
fn a_newer_schema_is_refused_naming_both_versions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("newer.db");
    drop(Store::open(&path, Arc::new(SystemClock), Limits::default()).unwrap());
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
            "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE; PRAGMA user_version = {};",
            puddle_store::SCHEMA_VERSION + 1
        ))
        .unwrap();
    let archive = gz_tar(&[file("puddle.db", &fs::read(&path).unwrap())]);
    let r = refusal(stage(&archive, root.path()));
    let text = r.to_string();
    assert!(
        text.contains(&format!("schema {}", puddle_store::SCHEMA_VERSION + 1)),
        "{text}"
    );
    assert!(
        text.contains(&format!("up to {}", puddle_store::SCHEMA_VERSION)),
        "{text}"
    );
    assert!(text.contains("update Puddle first"), "{text}");
    nothing_staged(root.path());
}

#[test]
fn an_unknown_manifest_format_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let manifest = br#"{"format":2,"puddle_version":"9.9.9","schema_version":1,"created_at_ms":0}"#;
    let archive = gz_tar(&[file(BACKUP_MANIFEST, manifest), file("puddle.db", &db)]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::UnknownFormat(2));
    nothing_staged(root.path());
}

#[test]
fn a_manifest_that_is_not_json_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let archive = gz_tar(&[file(BACKUP_MANIFEST, b"not json"), file("puddle.db", &db)]);
    let r = refusal(stage(&archive, root.path()));
    assert!(matches!(r, Refusal::BadManifest(_)), "{r}");
}

#[test]
fn the_same_name_twice_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let archive = gz_tar(&[file("puddle.db", &db), file("puddle.db", &db)]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::Duplicate("puddle.db".into()));
}

#[test]
fn a_backup_folder_is_staged_like_an_archive() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(root.path().join("data"));
    fs::create_dir_all(paths.data()).unwrap();
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    let folder = root.path().join("folder");
    crate::backup(&paths, &folder).unwrap();
    let staged = stage_folder(&folder, &root.path().join("staging")).unwrap();
    assert!(staged.dir.join(BACKUP_DATABASE).is_file());
    assert!(staged.manifest.is_some());
    assert!(
        folder.join(BACKUP_DATABASE).is_file(),
        "the folder is only read"
    );
}

#[cfg(unix)]
#[test]
fn a_folder_with_a_link_or_an_extra_name_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let folder = root.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("puddle.db"), &db).unwrap();
    std::os::unix::fs::symlink("/etc", folder.join("settings")).unwrap();
    let r = refusal(stage_folder(&folder, &root.path().join("staging")));
    assert_eq!(r, Refusal::Link("settings".into()));
    fs::remove_file(folder.join("settings")).unwrap();
    fs::write(folder.join("extra.txt"), b"x").unwrap();
    let r = refusal(stage_folder(&folder, &root.path().join("staging")));
    assert_eq!(r, Refusal::UnknownEntry("extra.txt".into()));
    nothing_staged(root.path());
}

#[cfg(unix)]
#[test]
fn staged_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let staged = stage(&real_archive(root.path()), root.path()).unwrap();
    for path in [staged.dir.clone(), staged.dir.join(BACKUP_DATABASE)] {
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o077;
        assert_eq!(mode, 0, "{} is open to others", path.display());
    }
}

fn data_folder(root: &Path, name: &str) -> HostPaths {
    let paths = HostPaths::new(root.join(name));
    fs::create_dir_all(paths.settings()).unwrap();
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    paths
}

fn sql(paths: &HostPaths, statements: &str) {
    rusqlite::Connection::open(paths.store())
        .unwrap()
        .execute_batch(statements)
        .unwrap();
}

#[test]
fn the_preview_counts_rules_workspaces_and_settings_against_this_machine() {
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(root.path(), "there");
    let here = data_folder(root.path(), "here");
    let rule = |host: &str| {
        format!(
            "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id) \
             VALUES ('global', 'exact', '{host}', 'allow', 1, 'cli', 1);"
        )
    };
    sql(
        &there,
        &[rule("a.example.org"), rule("b.example.org")].concat(),
    );
    sql(
        &here,
        &[rule("a.example.org"), rule("c.example.org")].concat(),
    );
    fs::write(
        there.workspace_book(),
        br#"{"version":4,"workspaces":[{"id":"w-one","name":"One"},{"id":"w-two","name":"Two"}]}"#,
    )
    .unwrap();
    fs::write(
        here.workspace_book(),
        br#"{"version":4,"workspaces":[{"id":"w-one","name":"One"}]}"#,
    )
    .unwrap();
    fs::write(
        there.settings().join("global.json"),
        br#"{"version":1,"a":1,"b":2}"#,
    )
    .unwrap();
    fs::write(
        here.settings().join("global.json"),
        br#"{"version":1,"a":1,"b":3,"c":4}"#,
    )
    .unwrap();

    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let staged = stage_archive(&bytes[..], &here.restore_staging()).unwrap();
    let facts = facts(&here, &staged).unwrap();

    let get = |name| *facts.parts.iter().find(|p| p.name == name).unwrap();
    let rules = get(RestorePartName::Rules);
    assert_eq!(
        (rules.in_backup, rules.add, rules.same, rules.only_here),
        (2, 1, 1, 1)
    );
    let workspaces = get(RestorePartName::Workspaces);
    assert_eq!((workspaces.add, workspaces.same), (1, 1));
    // `a` is equal, `b` differs, `c` exists only here.
    let settings = get(RestorePartName::Settings);
    assert_eq!(
        (
            settings.in_backup,
            settings.same,
            settings.differ,
            settings.only_here
        ),
        (2, 1, 1, 1)
    );
    assert_eq!(facts.staging_id, staged.id);
    assert_eq!(facts.format, 1);
    assert!(facts.puddle_version.is_some());
    assert_eq!(
        facts.workspaces,
        vec![
            ("w-one".into(), "One".into()),
            ("w-two".into(), "Two".into())
        ]
    );
}

#[test]
fn staging_never_touches_the_present_data_folder() {
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(root.path(), "there");
    let here = data_folder(root.path(), "here");
    fs::write(here.settings().join("global.json"), br#"{"version":1}"#).unwrap();
    let before = (
        fs::read(here.store()).unwrap(),
        fs::read(here.settings().join("global.json")).unwrap(),
    );
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let staged = stage_archive(&bytes[..], &here.restore_staging()).unwrap();
    facts(&here, &staged).unwrap();
    let after = (
        fs::read(here.store()).unwrap(),
        fs::read(here.settings().join("global.json")).unwrap(),
    );
    assert_eq!(before, after);
}

#[test]
fn old_staging_folders_are_swept_and_fresh_ones_stay() {
    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("staging");
    fs::create_dir_all(staging.join("fresh")).unwrap();
    fs::create_dir_all(staging.join("stale")).unwrap();
    let day_ago =
        std::time::SystemTime::now() - STAGING_LIFETIME - std::time::Duration::from_secs(60);
    File::open(staging.join("stale"))
        .unwrap()
        .set_modified(day_ago)
        .unwrap();
    assert_eq!(sweep_staging(&staging, STAGING_LIFETIME), 1);
    assert!(staging.join("fresh").is_dir());
    assert!(!staging.join("stale").exists());
    assert_eq!(
        sweep_staging(&root.path().join("missing"), STAGING_LIFETIME),
        0
    );
    assert_eq!(STAGING_LIFETIME.as_secs(), 24 * 3600);
}

#[test]
fn staging_ids_are_32_lowercase_hex_and_nothing_else() {
    let root = Path::new("/r");
    assert!(staging_dir(root, &"ab".repeat(16)).is_some());
    for bad in [
        "",
        "../x",
        &"AB".repeat(16),
        &"ab".repeat(15),
        &"zz".repeat(16),
    ] {
        assert!(staging_dir(root, bad).is_none(), "{bad}");
    }
}

#[tokio::test]
async fn the_service_stages_an_upload_and_refuses_a_bad_file_leaving_nothing() {
    use futures_util::StreamExt as _;
    use puddle_api::RestoreService as _;
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(root.path(), "there");
    let here = data_folder(root.path(), "here");
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let service = DataFolderRestore::new(here.clone(), Arc::default());
    let pieces: Vec<io::Result<Vec<u8>>> = bytes.chunks(1000).map(|c| Ok(c.to_vec())).collect();
    let facts = service
        .preview(futures_util::stream::iter(pieces).boxed())
        .await
        .unwrap();
    assert!(here.restore_staging().join(&facts.staging_id).is_dir());

    let refused = service
        .preview(futures_util::stream::iter([Ok(b"not a backup".to_vec())]).boxed())
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, RestoreServiceError::Refused(m) if m.contains("not a gzip")),
        "{refused:?}"
    );
    assert_eq!(fs::read_dir(here.restore_staging()).unwrap().count(), 1);
}
