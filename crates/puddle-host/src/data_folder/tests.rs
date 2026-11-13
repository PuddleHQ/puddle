// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fs;
use std::sync::Arc;

use puddle_store::{Limits, Store, SystemClock};

use super::*;

fn is_owner_only(path: &Path) -> bool {
    private::check(&fs::File::open(path).unwrap()).is_ok()
}

#[test]
fn a_new_install_gets_an_owner_only_folder_and_database_file() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(root.path().join("a").join("puddle"));
    make_owner_only(&paths).unwrap();
    assert!(is_owner_only(&paths.store()));
    assert_eq!(private::tighten_dir(paths.data()).unwrap(), None);
}

#[test]
fn the_database_opens_on_the_pre_made_file_and_its_side_files_stay_private() {
    let root = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(root.path().join("puddle"));
    make_owner_only(&paths).unwrap();
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let mut name = paths.store().into_os_string();
        name.push(suffix);
        let name = PathBuf::from(name);
        // SQLite in WAL mode keeps both side files while a connection is open.
        assert!(is_owner_only(&name), "{}", name.display());
    }
    drop(store);
    // A second start on the now-populated folder changes nothing.
    make_owner_only(&paths).unwrap();
}

#[test]
fn an_existing_open_install_is_fixed_and_its_data_is_kept() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("puddle");
    fs::create_dir(&data).unwrap();
    let paths = HostPaths::new(&data);
    for name in ["puddle.db", "puddle.db-wal", "puddle.db-shm"] {
        fs::write(data.join(name), name).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // flake-ok: a folder's mode, nothing is executed
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
        for name in ["puddle.db", "puddle.db-wal", "puddle.db-shm"] {
            fs::set_permissions(data.join(name), fs::Permissions::from_mode(0o644)).unwrap();
        }
    }
    // On Windows the folder under the temporary location inherits the profile's list.
    assert!(!is_owner_only(&paths.store()));
    make_owner_only(&paths).unwrap();
    for name in ["puddle.db", "puddle.db-wal", "puddle.db-shm"] {
        assert!(is_owner_only(&data.join(name)), "{name}");
        assert_eq!(fs::read_to_string(data.join(name)).unwrap(), name);
    }
    assert_eq!(private::tighten_dir(&data).unwrap(), None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn something_that_cannot_be_fixed_stops_the_start_with_a_clear_message() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("puddle");
    fs::create_dir(&data).unwrap();
    // A folder where the database file should be: it can't be tightened as a file.
    fs::create_dir(data.join("puddle.db")).unwrap();
    let err = make_owner_only(&HostPaths::new(&data))
        .unwrap_err()
        .to_string();
    assert!(err.contains("puddle.db"), "{err}");
    assert!(err.contains("will not run with wider permissions"), "{err}");

    // A file where the data folder should be.
    let file = root.path().join("not-a-folder");
    fs::write(&file, b"x").unwrap();
    let err = make_owner_only(&HostPaths::new(&file))
        .unwrap_err()
        .to_string();
    assert!(err.contains("not-a-folder"), "{err}");
}

/// msb creates its catalog with the process's default permissions (world-readable before msb
/// 0.7.12, advisory GHSA-cvr8-2329-j4xw), so the data folder around the msb home is what
/// keeps other accounts out of it.
#[test]
fn the_msb_catalog_is_unreachable_for_other_accounts_inside_the_data_folder() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("a").join("puddle");
    let exe = root.path().join("app").join("puddle");
    let config = crate::HostConfig::installed(&exe, &data).unwrap();
    let home = config.layout.home().to_path_buf();
    assert_eq!(home, data.join("msb"));

    // The start's order: lock the folder (creates it), tighten it, then msb opens its home.
    let _lock = puddle_fs::DataLock::acquire(&data).unwrap();
    make_owner_only(&config.paths).unwrap();
    let db_dir = home.join("db");
    fs::create_dir_all(&db_dir).unwrap();
    let catalog = db_dir.join("msb.db");
    fs::write(&catalog, b"catalog").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // The catalog as msb before 0.7.12 left it.
        fs::set_permissions(&catalog, fs::Permissions::from_mode(0o644)).unwrap();
        // The data folder and the folders Puddle made above it are closed to group and
        // other, so the catalog cannot be reached by path.
        let puddle_made = data.ancestors().take_while(|dir| *dir != root.path());
        for dir in puddle_made {
            assert_eq!(mode(dir), 0o700, "{}", dir.display());
        }
    }
    #[cfg(windows)]
    {
        // The ACL is inherited: the catalog msb made, and its folders, have the owner's
        // entry alone.
        private::check(&fs::File::open(&catalog).unwrap()).unwrap();
        assert_eq!(private::tighten_dir(&db_dir).unwrap(), None);
        assert_eq!(private::tighten_dir(&home).unwrap(), None);
        assert_eq!(private::tighten_dir(&data).unwrap(), None);
    }
}

#[test]
fn the_log_line_describes_both_kinds_of_exposure() {
    assert!(describe(&Exposed::Mode(0o755)).contains("mode 755"));
    assert!(describe(&Exposed::Acl("entry 1".into())).contains("entry 1"));
}
