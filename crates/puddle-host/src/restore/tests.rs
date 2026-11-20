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

/// Sets a folder's modified time. Windows opens a folder only with backup semantics, and a
/// write handle is what `set_modified` needs there; other systems open a folder like a file.
fn set_folder_modified(folder: &Path, when: std::time::SystemTime) {
    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(folder)
            .unwrap()
    };
    #[cfg(not(windows))]
    let handle = File::open(folder).unwrap();
    handle.set_modified(when).unwrap();
}

#[test]
fn old_staging_folders_are_swept_and_fresh_ones_stay() {
    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("staging");
    fs::create_dir_all(staging.join("fresh")).unwrap();
    fs::create_dir_all(staging.join("stale")).unwrap();
    let day_ago =
        std::time::SystemTime::now() - STAGING_LIFETIME - std::time::Duration::from_secs(60);
    set_folder_modified(&staging.join("stale"), day_ago);
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

fn service_for(here: &HostPaths) -> DataFolderRestore {
    DataFolderRestore::new(here.clone(), Arc::default())
}

fn upload_of(bytes: Vec<u8>) -> puddle_api::UploadStream {
    use futures_util::StreamExt as _;
    futures_util::stream::iter([Ok(bytes)]).boxed()
}

#[test]
fn a_dot_path_and_a_folder_entry_are_the_same_as_the_plain_one() {
    let root = tempfile::tempdir().unwrap();
    let db = good_db(root.path());
    let archive = gz_tar(&[
        Entry {
            name: "./",
            kind: tar::EntryType::Directory,
            body: b"",
            link: None,
        },
        Entry {
            name: "settings/",
            kind: tar::EntryType::Directory,
            body: b"",
            link: None,
        },
        file("./puddle.db", &db),
        file("settings/global.json", b"{}"),
    ]);
    let staged = stage(&archive, root.path()).unwrap();
    assert!(staged.dir.join("puddle.db").is_file());
    assert!(staged.dir.join("settings").join("global.json").is_file());
}

#[test]
fn a_file_where_a_folder_must_be_is_an_io_error_not_a_refusal() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[
        file("settings", b"a file"),
        file("settings/global.json", b"{}"),
    ]);
    let err = stage(&archive, root.path()).unwrap_err();
    assert!(matches!(err, RestoreError::Io { .. }), "{err}");
    nothing_staged(root.path());
}

#[test]
fn a_special_entry_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let archive = gz_tar(&[Entry {
        name: "settings/pipe",
        kind: tar::EntryType::Fifo,
        body: b"",
        link: None,
    }]);
    let r = refusal(stage(&archive, root.path()));
    assert_eq!(r, Refusal::UnknownEntry("settings/pipe".into()));
}

#[test]
fn a_folder_over_the_limit_is_refused_and_a_nested_one_is_copied() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("folder");
    fs::create_dir_all(folder.join("settings").join("workspaces")).unwrap();
    fs::write(folder.join("puddle.db"), good_db(root.path())).unwrap();
    fs::write(
        folder.join("settings").join("workspaces").join("a.json"),
        b"{}",
    )
    .unwrap();
    let staging = root.path().join("staging");
    let staged = stage_folder(&folder, &staging).unwrap();
    assert!(staged.dir.join("settings/workspaces/a.json").is_file());
    let r = refusal(staged_in(&staging, |dir| copy_folder(&folder, dir, 10)));
    assert_eq!(r, Refusal::TooLarge);
}

#[cfg(unix)]
#[test]
fn a_special_file_in_a_folder_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("folder");
    fs::create_dir(&folder).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(folder.join("puddle.db")).unwrap();
    let r = refusal(stage_folder(&folder, &root.path().join("staging")));
    assert_eq!(r, Refusal::UnknownEntry("puddle.db".into()));
}

#[test]
fn a_manifest_that_cannot_be_read_is_an_io_error() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("folder");
    fs::create_dir_all(folder.join("manifest.json")).unwrap();
    fs::write(folder.join("puddle.db"), good_db(root.path())).unwrap();
    let err = stage_folder(&folder, &root.path().join("staging")).unwrap_err();
    assert!(matches!(err, RestoreError::Io { .. }), "{err}");
}

#[test]
fn settings_are_read_by_key_with_nested_folders_odd_files_and_no_temp_files() {
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings");
    fs::create_dir_all(settings.join("workspaces")).unwrap();
    fs::write(settings.join("global.json"), br#"{"version":1,"a":1}"#).unwrap();
    fs::write(settings.join("workspaces").join("w.json"), br#"{"b":2}"#).unwrap();
    fs::write(settings.join("notes.txt"), b"plain text").unwrap();
    fs::write(settings.join(".global.json.9.tmp"), b"half").unwrap();
    let entries = settings_entries(&settings);
    let keys: Vec<&str> = entries.keys().map(String::as_str).collect();
    assert_eq!(keys, ["global.json#a", "notes.txt", "workspaces/w.json#b"]);
    assert_eq!(entries["notes.txt"], "plain text");
    assert!(settings_entries(&root.path().join("missing")).is_empty());
}

#[test]
fn a_workspace_list_that_is_not_json_names_no_workspaces() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("workspaces.json");
    assert_eq!(workspace_names(&path).len(), 0);
    fs::write(&path, b"{ not json").unwrap();
    assert_eq!(workspace_names(&path).len(), 0);
    fs::write(&path, br#"{"workspaces":[{"id":"ws-a"},{"name":"no id"}]}"#).unwrap();
    assert_eq!(
        workspace_names(&path),
        [("ws-a".to_owned(), "ws-a".to_owned())]
    );
}

#[test]
fn the_preview_lists_the_secret_variables_of_the_backup() {
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(root.path(), "there");
    let here = data_folder(root.path(), "here");
    sql(
        &there,
        "INSERT INTO env_vars (workspace_id, name, kind, secret_id, hosts, changed_at, user_id)
         VALUES (NULL, 'TOKEN', 'secret', 'sid-1', 'example.org', 1, 1);",
    );
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let staged = stage_archive(&bytes[..], &here.restore_staging()).unwrap();
    let got = facts(&here, &staged).unwrap();
    assert_eq!(
        got.secret_refs,
        [SecretRef {
            workspace_id: None,
            name: "TOKEN".into(),
            secret_id: "sid-1".into()
        }]
    );
}

#[tokio::test]
async fn a_machine_with_no_database_fails_the_preview_as_a_failure_not_a_refusal() {
    use puddle_api::RestoreService as _;
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(root.path(), "there");
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let nowhere = HostPaths::new(root.path().join("nowhere"));
    let err = service_for(&nowhere)
        .preview(upload_of(bytes))
        .await
        .unwrap_err();
    assert!(matches!(err, RestoreServiceError::Failed(_)), "{err:?}");
    // The staged copy of a failed preview is not left behind.
    assert_eq!(fs::read_dir(nowhere.restore_staging()).unwrap().count(), 0);
}

#[tokio::test]
async fn an_upload_that_stops_is_a_failure_that_says_so() {
    use futures_util::StreamExt as _;
    use puddle_api::RestoreService as _;
    let root = tempfile::tempdir().unwrap();
    let here = data_folder(root.path(), "here");
    let broken = futures_util::stream::iter([Err(io::Error::other("connection reset"))]).boxed();
    let err = service_for(&here).preview(broken).await.unwrap_err();
    assert!(
        matches!(&err, RestoreServiceError::Failed(m) if m.contains("the upload stopped")),
        "{err:?}"
    );
}

#[tokio::test]
async fn confirming_names_what_is_wrong_in_the_words_of_the_failure() {
    use puddle_api::RestoreService as _;
    use puddle_api::wire::RestoreMode;
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(root.path(), "there");
    let here = data_folder(root.path(), "here");
    let service = service_for(&here);
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let id = service.preview(upload_of(bytes)).await.unwrap().staging_id;

    // Not staged.
    let err = service
        .apply(&"0".repeat(32), RestoreMode::Add)
        .await
        .unwrap_err();
    assert!(matches!(err, RestoreServiceError::NotStaged(_)), "{err:?}");
    // The safety copy cannot be written: a failure of this machine.
    fs::write(here.safety_copies(), b"in the way").unwrap();
    let err = service.apply(&id, RestoreMode::Add).await.unwrap_err();
    assert!(matches!(err, RestoreServiceError::Failed(_)), "{err:?}");
    fs::remove_file(here.safety_copies()).unwrap();
    // A restore already waits: the user's to read.
    service.apply(&id, RestoreMode::Replace).await.unwrap();
    let again = service.apply(&id, RestoreMode::Add).await.unwrap_err();
    assert!(
        matches!(
            again,
            RestoreServiceError::NotStaged(_) | RestoreServiceError::Refused(_)
        ),
        "{again:?}"
    );
    // A second staged copy meets the waiting one.
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let second = service.preview(upload_of(bytes)).await.unwrap().staging_id;
    let waiting = service.apply(&second, RestoreMode::Add).await.unwrap_err();
    assert!(
        matches!(&waiting, RestoreServiceError::Refused(m) if m.contains("already waiting")),
        "{waiting:?}"
    );
}

#[tokio::test]
async fn the_result_is_read_in_either_mode_and_an_unreadable_one_is_a_failure() {
    use puddle_api::RestoreService as _;
    let root = tempfile::tempdir().unwrap();
    let here = data_folder(root.path(), "here");
    let service = service_for(&here);
    assert!(service.result().await.unwrap().is_none());
    for (mode, wire) in [
        (ApplyMode::Add, puddle_api::wire::RestoreMode::Add),
        (ApplyMode::Replace, puddle_api::wire::RestoreMode::Replace),
    ] {
        let outcome = apply::RestoreOutcome {
            applied: true,
            mode,
            safety_copy: "20231114T221320Z".into(),
            at_ms: 5,
            message: "done".into(),
        };
        fs::write(here.restore_result(), serde_json::to_vec(&outcome).unwrap()).unwrap();
        let got = service.result().await.unwrap().unwrap();
        assert_eq!(
            (got.mode, got.at_ms, got.message.as_str()),
            (wire, 5, "done")
        );
    }
    fs::write(here.restore_result(), b"{ not json").unwrap();
    assert!(matches!(
        service.result().await.unwrap_err(),
        RestoreServiceError::Failed(_)
    ));
    fs::remove_file(here.restore_result()).unwrap();
    // A folder where the file should be cannot be read or removed.
    fs::create_dir(here.restore_result()).unwrap();
    assert!(matches!(
        service.result().await.unwrap_err(),
        RestoreServiceError::Failed(_)
    ));
    assert!(matches!(
        service.dismiss_result().await.unwrap_err(),
        RestoreServiceError::Failed(_)
    ));
    fs::remove_dir(here.restore_result()).unwrap();
    service.dismiss_result().await.unwrap();
}

#[tokio::test]
async fn safety_copies_are_listed_newest_first_and_only_a_real_one_is_deleted() {
    use puddle_api::RestoreService as _;
    let root = tempfile::tempdir().unwrap();
    let here = data_folder(root.path(), "here");
    let service = service_for(&here);
    assert_eq!(service.safety_copies().await.unwrap().len(), 0);
    for name in ["20231001T000000Z", "20231114T221320Z"] {
        let dir = here.safety_copies().join(name);
        fs::create_dir_all(dir.join("settings")).unwrap();
        fs::write(dir.join("puddle.db"), vec![0_u8; 100]).unwrap();
        fs::write(dir.join("settings").join("global.json"), b"{}").unwrap();
    }
    // Neither a stray file nor a folder with another name is a safety copy.
    fs::write(here.safety_copies().join("20231201T000000Z"), b"a file").unwrap();
    fs::create_dir(here.safety_copies().join("not-a-copy")).unwrap();
    let copies = service.safety_copies().await.unwrap();
    let names: Vec<&str> = copies.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["20231114T221320Z", "20231001T000000Z"]);
    assert_eq!(copies[0].size_bytes, 102);

    service
        .delete_safety_copy("20231001T000000Z")
        .await
        .unwrap();
    assert!(!here.safety_copies().join("20231001T000000Z").exists());
    for bad in [
        "20231001T000000Z",
        "../here",
        "not-a-copy",
        "20231201T000000Z",
    ] {
        let err = service.delete_safety_copy(bad).await.unwrap_err();
        assert!(
            matches!(err, RestoreServiceError::NotStaged(_)),
            "{bad}: {err:?}"
        );
    }
    assert!(here.safety_copies().join("not-a-copy").exists());

    // A file where the safety copies' folder should be cannot be listed.
    fs::remove_dir_all(here.safety_copies()).unwrap();
    fs::write(here.safety_copies(), b"in the way").unwrap();
    assert!(matches!(
        service.safety_copies().await.unwrap_err(),
        RestoreServiceError::Failed(_)
    ));
}

#[test]
fn a_safety_copy_name_is_a_utc_time_and_the_summary_reads_in_both_modes() {
    assert!(is_safety_name("20231114T221320Z"));
    for bad in [
        "",
        "20231114221320Z",
        "20231114T22132Z",
        "2023111aT221320Z",
        "20231114T221320z",
    ] {
        assert!(!is_safety_name(bad), "{bad}");
    }
    let part = |add, differ, only_here| RestorePart {
        name: RestorePartName::Rules,
        in_backup: 0,
        add,
        differ,
        same: 0,
        only_here,
    };
    let parts = [part(2, 1, 3), part(1, 1, 0)];
    assert_eq!(
        summarise(ApplyMode::Add, &parts),
        "Added 3 entries that were missing; kept your version of 2 that differ."
    );
    assert_eq!(
        summarise(ApplyMode::Replace, &parts),
        "Added 3, replaced 2 and dropped 3 entries that were not in the backup."
    );
}

#[test]
fn a_staging_id_is_32_hex_digits_and_a_failing_random_source_is_an_error() {
    let id = staging_id(|raw| {
        raw.fill(0xab);
        Ok(())
    })
    .unwrap();
    assert_eq!(id, "ab".repeat(16));
    let err = staging_id(|_| Err(getrandom::Error::UNSUPPORTED)).unwrap_err();
    assert!(matches!(err, RestoreError::Io { .. }), "{err}");
}

#[test]
fn a_part_that_cannot_be_created_is_an_io_error_and_a_taken_name_is_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    // The folder above it is missing.
    let err = create_part(&dir.path().join("missing").join("x"), "x").unwrap_err();
    assert!(matches!(err, RestoreError::Io { .. }), "{err}");
    let taken = dir.path().join("taken");
    fs::write(&taken, b"x").unwrap();
    let err = create_part(&taken, "taken").unwrap_err();
    assert!(
        matches!(err, RestoreError::Refused(Refusal::Duplicate(_))),
        "{err}"
    );
}

#[test]
fn a_safety_copy_that_cannot_be_removed_is_a_failure_naming_it() {
    let dir = tempfile::tempdir().unwrap();
    let err = remove_copy(&dir.path().join("gone")).unwrap_err();
    assert!(
        matches!(&err, RestoreServiceError::Failed(m) if m.contains("cannot delete")),
        "{err:?}"
    );
}
