// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn name(n: &str) -> WorkspaceId {
    WorkspaceId::new(n).unwrap()
}

fn entry(endpoint: &str) -> Entry {
    Entry {
        endpoint: PathBuf::from(endpoint),
        user: SessionUser::Root,
    }
}

fn context<'a>(bridge: &'a Path, identity: &'a Path, dir: &'a Path) -> Context<'a> {
    Context {
        bridge,
        identity,
        known_hosts_dir: dir,
    }
}

#[cfg(unix)]
#[test]
fn an_entry_names_the_bridge_the_endpoint_and_only_puddles_key() {
    let mut entries = BTreeMap::new();
    entries.insert(name("web"), entry("/run/puddle/ep-1"));
    entries.insert(
        name("api"),
        Entry {
            endpoint: PathBuf::from("/run/puddle/ep-2"),
            user: SessionUser::Agent,
        },
    );
    let text = render(
        &entries,
        &context(
            Path::new("/opt/puddle/puddle"),
            Path::new("/data/ssh/login_ed25519"),
            Path::new("/data/ssh/known_hosts"),
        ),
    )
    .unwrap();
    for want in [
        "Host puddle-web\n",
        "    User root\n",
        "    ProxyCommand \"/opt/puddle/puddle\" ssh-bridge \"/run/puddle/ep-1\"\n",
        "    IdentityFile \"/data/ssh/login_ed25519\"\n",
        "    IdentitiesOnly yes\n",
        "    IdentityAgent none\n",
        "    ControlMaster no\n",
        "    ControlPath none\n",
        "    UserKnownHostsFile \"/data/ssh/known_hosts/web\"\n",
        "    HostKeyAlias puddle-web\n",
    ] {
        assert!(text.contains(want), "{want:?} in\n{text}");
    }
    assert_eq!(text.matches("Host puddle-").count(), 2);
    let api =
        &text[text.find("Host puddle-api\n").unwrap()..text.find("Host puddle-web\n").unwrap()];
    assert!(api.contains("    User agent\n"), "{api}");
    assert!(!api.contains("User root"), "{api}");
}

/// The Windows shape is checked against a real `ssh.exe` (OpenSSH for Windows 9.5): it runs the
/// line as `exec <line>` through its own `CreateProcess`, and a program in a folder with a
/// space and an endpoint with backslashes both arrive intact with four quotes and no outer pair
/// (a doubled outer pair is refused with `CreateProcessW failed error:87`).
#[cfg(windows)]
#[test]
fn the_windows_entry_quotes_the_program_and_the_endpoint_and_nothing_around_them() {
    let mut entries = BTreeMap::new();
    entries.insert(name("web"), entry(r"\\.\pipe\puddle-ab12"));
    let text = render(
        &entries,
        &context(
            Path::new(r"C:\Program Files\Puddle\puddle.exe"),
            Path::new(r"C:\Users\Ada\AppData\Local\Puddle\ssh\login_ed25519"),
            Path::new(r"C:\Users\Ada\AppData\Local\Puddle\ssh\known_hosts"),
        ),
    )
    .unwrap();
    for want in [
        "    ProxyCommand \"C:\\Program Files\\Puddle\\puddle.exe\" ssh-bridge \"\\\\.\\pipe\\puddle-ab12\"\n",
        "    IdentityFile \"C:/Users/Ada/AppData/Local/Puddle/ssh/login_ed25519\"\n",
        "    UserKnownHostsFile \"C:/Users/Ada/AppData/Local/Puddle/ssh/known_hosts/web\"\n",
    ] {
        assert!(text.contains(want), "{want:?} in\n{text}");
    }
}

#[test]
fn no_entries_is_just_the_header() {
    let text = render(
        &BTreeMap::new(),
        &context(Path::new("/p"), Path::new("/k"), Path::new("/h")),
    )
    .unwrap();
    assert_eq!(text, HEADER);
    assert!(!text.contains("Host "));
}

#[test]
fn a_value_that_could_end_a_word_or_start_a_line_is_refused() {
    let ok = context(Path::new("/p"), Path::new("/k"), Path::new("/h"));
    for hostile in [
        "/tmp/a\"b",
        "/tmp/a\nHost evil",
        "/tmp/a\rProxyCommand x",
        "/tmp/$(id)",
        "/tmp/`id`",
        "/tmp/%h",
        "/tmp/a\0b",
    ] {
        let mut entries = BTreeMap::new();
        entries.insert(name("web"), entry(hostile));
        let err = render(&entries, &ok).unwrap_err().to_string();
        assert!(err.contains("cannot hold safely"), "{hostile:?}: {err}");
        let identity = context(Path::new("/p"), Path::new(hostile), Path::new("/h"));
        let mut entries = BTreeMap::new();
        entries.insert(name("web"), entry("/ep"));
        assert!(render(&entries, &identity).is_err(), "identity {hostile:?}");
        let program = context(Path::new(hostile), Path::new("/k"), Path::new("/h"));
        assert!(render(&entries, &program).is_err(), "program {hostile:?}");
    }
}

#[cfg(unix)]
mod files {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn setup(dir: &Path, with_user: bool) -> (SshSetup, PathBuf) {
        let user = dir.join("home/.ssh/config");
        let settings = SshSettings {
            bridge_program: Some(PathBuf::from("/opt/puddle/puddle")),
            user_config: with_user.then(|| user.clone()),
        };
        (SshSetup::open(&dir.join("data"), &settings), user)
    }

    #[test]
    fn publishing_writes_the_entry_and_one_include_line_and_withdrawing_removes_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, user) = setup(dir.path(), true);
        assert!(ssh.authorized_key().is_some());
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        let puddle_file = std::fs::read_to_string(ssh.config_path()).unwrap();
        assert!(puddle_file.contains("Host puddle-web"));
        let user_text = std::fs::read_to_string(&user).unwrap();
        assert_eq!(
            user_text,
            format!("Include \"{}\"\n", ssh.config_path().display())
        );
        ssh.withdraw(&name("web")).unwrap();
        assert!(
            !std::fs::read_to_string(ssh.config_path())
                .unwrap()
                .contains("Host ")
        );
        assert_eq!(std::fs::read_to_string(&user).unwrap(), user_text);
        assert_eq!(ssh.endpoint(&name("web")), None);
    }

    #[test]
    fn a_withdraw_that_cannot_write_says_so_and_the_next_write_drops_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        // A folder with a file in it where Puddle's file should be: the swap fails.
        let file = ssh.config_path();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir_all(file.join("in")).unwrap();
        let err = ssh.withdraw(&name("web")).unwrap_err().to_string();
        assert!(
            err.contains("puddle-web") && err.contains("cannot take"),
            "{err}"
        );
        assert_eq!(ssh.endpoint(&name("web")), None);
        std::fs::remove_dir_all(&file).unwrap();
        ssh.publish(&name("db"), Path::new("/run/ep-2"), SessionUser::Root)
            .unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("Host puddle-db") && !text.contains("puddle-web"));
    }

    #[test]
    fn withdrawing_a_workspace_without_an_entry_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        ssh.withdraw(&name("web")).unwrap();
        assert!(!ssh.config_path().exists());
    }

    #[test]
    fn an_entry_follows_its_endpoint_and_forgets_the_old_host_key() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        let known = ssh.known_hosts(&name("web"));
        std::fs::write(&known, "puddle-web ssh-ed25519 AAAA old\n").unwrap();
        ssh.publish(&name("web"), Path::new("/run/ep-2"), SessionUser::Root)
            .unwrap();
        assert!(!known.exists());
        let text = std::fs::read_to_string(ssh.config_path()).unwrap();
        assert!(
            text.contains("/run/ep-2") && !text.contains("/run/ep-1"),
            "{text}"
        );
    }

    #[test]
    fn a_stale_file_from_a_crashed_run_is_emptied_at_start() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        drop(ssh);
        let (again, _) = setup(dir.path(), false);
        assert!(
            !std::fs::read_to_string(again.config_path())
                .unwrap()
                .contains("Host ")
        );
    }

    #[test]
    fn a_failed_publish_leaves_the_entries_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        let err = ssh
            .publish(&name("web"), Path::new("/run/\"x"), SessionUser::Root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot hold"));
        assert_eq!(ssh.endpoint(&name("web")), Some(PathBuf::from("/run/ep-1")));
        assert!(
            ssh.publish(&name("db"), Path::new("/run/\n"), SessionUser::Root)
                .is_err()
        );
        assert_eq!(ssh.endpoint(&name("db")), None);
    }

    #[test]
    fn a_host_key_file_that_cannot_be_removed_is_an_error_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        // A folder where the file should be: removing it as a file fails.
        std::fs::create_dir_all(ssh.known_hosts(&name("web"))).unwrap();
        let err = ssh
            .publish(&name("web"), Path::new("/run/ep"), SessionUser::Root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot forget the old host key"), "{err}");
        assert_eq!(ssh.endpoint(&name("web")), None);
    }

    #[test]
    fn a_start_without_key_or_program_still_empties_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let (first, _) = setup(dir.path(), false);
        first
            .publish(&name("web"), Path::new("/run/ep"), SessionUser::Root)
            .unwrap();
        drop(first);
        let again = SshSetup::open(&data, &SshSettings::default());
        assert_eq!(
            std::fs::read_to_string(again.config_path()).unwrap(),
            HEADER
        );
    }

    #[test]
    fn a_config_file_that_cannot_be_written_says_which_one_and_leaves_no_entry() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        // A folder where the file belongs cannot be replaced by a file.
        std::fs::create_dir_all(ssh.config_path()).unwrap();
        let err = ssh
            .publish(&name("web"), Path::new("/run/ep"), SessionUser::Root)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with(&format!("cannot write {}: ", ssh.config_path().display())),
            "{err}"
        );
        assert_eq!(ssh.endpoint(&name("web")), None);
    }

    #[test]
    fn emptying_the_file_of_a_setup_without_key_or_program_reports_a_failed_write() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = SshSetup::open(&dir.path().join("data"), &SshSettings::default());
        std::fs::create_dir_all(ssh.config_path()).unwrap();
        let err = ssh.write_config(&BTreeMap::new()).unwrap_err();
        assert!(matches!(err, SshConfigError::Io(_)), "{err}");
    }

    #[test]
    fn a_config_path_below_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        let err = ensure_include(&file.join("config"), &dir.path().join("p"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("os error"), "{err}");
    }

    #[test]
    fn without_a_bridge_program_there_is_no_entry_and_it_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = SshSetup::open(&dir.path().join("data"), &SshSettings::default());
        let err = ssh
            .publish(&name("web"), Path::new("/run/ep"), SessionUser::Root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no `puddle` program"), "{err}");
    }

    #[test]
    fn an_unmakeable_key_is_kept_and_blocks_entries() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("data");
        std::fs::write(&blocker, "a file where the folder should be").unwrap();
        let settings = SshSettings {
            bridge_program: Some("/p".into()),
            user_config: None,
        };
        let ssh = SshSetup::open(&blocker, &settings);
        assert!(ssh.authorized_key().is_none());
        let err = ssh
            .publish(&name("web"), Path::new("/e"), SessionUser::Root)
            .unwrap_err()
            .to_string();
        assert!(err.contains("login key"), "{err}");
    }

    #[test]
    fn the_include_goes_first_and_the_rest_of_the_users_file_is_byte_for_byte_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        let original = b"Host *\n  ServerAliveInterval 30\r\n\xff not utf8\n";
        std::fs::write(&user, original).unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o640)).unwrap();
        let puddle = dir.path().join("own ssh config");
        ensure_include(&user, &puddle).unwrap();
        let now = std::fs::read(&user).unwrap();
        let line = format!("Include \"{}\"", puddle.display());
        assert!(now.starts_with(line.as_bytes()));
        assert!(
            now.ends_with(original),
            "{:?}",
            String::from_utf8_lossy(&now)
        );
        // The file has a CRLF, so the new line and the blank line after it end with one too.
        assert_eq!(now.len(), line.len() + 4 + original.len());
        assert_eq!(
            std::fs::metadata(&user).unwrap().permissions().mode() & 0o777,
            0o640
        );
        ensure_include(&user, &puddle).unwrap();
        assert_eq!(
            std::fs::read(&user).unwrap(),
            now,
            "a second call changes nothing"
        );
    }

    #[test]
    fn a_crlf_file_gets_a_crlf_line() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        std::fs::write(&user, "Host a\r\n  User x\r\n").unwrap();
        ensure_include(&user, &dir.path().join("p")).unwrap();
        let text = std::fs::read_to_string(&user).unwrap();
        assert!(text.contains("\"\r\n\r\nHost a"), "{text:?}");
    }

    #[test]
    fn a_symlinked_config_stays_a_symlink_and_its_target_changes() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles-config");
        std::fs::write(&real, "Host a\n").unwrap();
        let link = dir.path().join("config");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        ensure_include(&link, &dir.path().join("p")).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(
            std::fs::read_to_string(&real)
                .unwrap()
                .starts_with("Include ")
        );
    }

    #[test]
    fn a_missing_config_and_folder_are_made_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join(".ssh/config");
        ensure_include(&user, &dir.path().join("p")).unwrap();
        assert_eq!(
            std::fs::metadata(&user).unwrap().permissions().mode() & 0o077,
            0
        );
        assert_eq!(
            std::fs::metadata(user.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }

    #[test]
    fn a_top_level_include_the_user_already_has_is_enough_even_when_it_is_not_first() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        let puddle = dir.path().join("p");
        let text = format!(
            "# mine\nSendEnv LANG\n  Include \"{}\"\nHost a\n  User x\n",
            puddle.display()
        );
        std::fs::write(&user, &text).unwrap();
        ensure_include(&user, &puddle).unwrap();
        assert_eq!(std::fs::read_to_string(&user).unwrap(), text);
    }

    #[test]
    fn an_include_inside_a_host_or_match_block_does_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let puddle = dir.path().join("p");
        let line = format!("Include \"{}\"", puddle.display());
        for opener in [
            "Host a",
            "host a",
            "HOST=a",
            "Host\ta",
            "Match all",
            "match host a",
            "  Host *",
        ] {
            let user = dir.path().join("config");
            let original = format!("{opener}\n  {line}\n  User x\n");
            std::fs::write(&user, &original).unwrap();
            ensure_include(&user, &puddle).unwrap();
            let now = std::fs::read_to_string(&user).unwrap();
            assert_eq!(now, format!("{line}\n\n{original}"), "{opener}");
            ensure_include(&user, &puddle).unwrap();
            assert_eq!(std::fs::read_to_string(&user).unwrap(), now, "{opener}");
        }
    }

    #[test]
    fn a_word_that_only_starts_like_host_does_not_end_the_top_level() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        let puddle = dir.path().join("p");
        let text = format!(
            "HostbasedAuthentication no\nMatchless yes\nInclude \"{}\"\n",
            puddle.display()
        );
        std::fs::write(&user, &text).unwrap();
        ensure_include(&user, &puddle).unwrap();
        assert_eq!(std::fs::read_to_string(&user).unwrap(), text);
    }

    #[test]
    fn a_link_to_a_file_that_is_not_there_stays_and_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("config");
        let missing = dir.path().join("dotfiles/config");
        std::os::unix::fs::symlink(&missing, &link).unwrap();
        let err = ensure_include(&link, &dir.path().join("p"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("symbolic link"), "{err}");
        assert_eq!(std::fs::read_link(&link).unwrap(), missing);
        assert!(!missing.exists());
    }

    #[test]
    fn a_read_only_config_is_not_rewritten_but_one_that_has_the_line_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        let puddle = dir.path().join("p");
        std::fs::write(&user, "Host a\n").unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o444)).unwrap();
        let err = ensure_include(&user, &puddle).unwrap_err().to_string();
        assert!(err.contains("read-only"), "{err}");
        assert_eq!(std::fs::read_to_string(&user).unwrap(), "Host a\n");
        assert_eq!(
            std::fs::metadata(&user).unwrap().permissions().mode() & 0o777,
            0o444
        );
        let text = format!("Include \"{}\"\n", puddle.display());
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&user, &text).unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o444)).unwrap();
        ensure_include(&user, &puddle).unwrap();
    }

    #[test]
    fn a_config_saved_by_another_program_meanwhile_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        std::fs::write(&user, "Host a\n").unwrap();
        let seen = std::fs::metadata(&user).unwrap();
        // The other program's save: a different length.
        std::fs::write(&user, "Host a\nHost b\n").unwrap();
        let err = replace_keeping_permissions(&user, b"Include x\n", &seen)
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed by another program"), "{err}");
        assert_eq!(std::fs::read_to_string(&user).unwrap(), "Host a\nHost b\n");
        // Same length, later time.
        let seen = std::fs::metadata(&user).unwrap();
        std::fs::write(&user, "Host a\nHost c\n").unwrap();
        let later = seen.modified().unwrap() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&user)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(replace_keeping_permissions(&user, b"Include x\n", &seen).is_err());
        assert_eq!(std::fs::read_to_string(&user).unwrap(), "Host a\nHost c\n");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    }

    #[test]
    fn the_swap_keeps_the_files_own_mode_even_a_wide_one() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        std::fs::write(&user, "Host a\n").unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o644)).unwrap();
        ensure_include(&user, &dir.path().join("p")).unwrap();
        assert_eq!(
            std::fs::metadata(&user).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[test]
    fn the_same_endpoint_keeps_the_host_key_and_a_new_one_forgets_it() {
        let dir = tempfile::tempdir().unwrap();
        let (ssh, _) = setup(dir.path(), false);
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        let known = ssh.known_hosts(&name("web"));
        std::fs::write(&known, "puddle-web ssh-ed25519 AAAA key\n").unwrap();
        ssh.publish(&name("web"), Path::new("/run/ep-1"), SessionUser::Root)
            .unwrap();
        assert!(
            known.exists(),
            "an attach to the same endpoint keeps the key"
        );
        // Another workspace's publish does not touch it either.
        ssh.publish(&name("db"), Path::new("/run/ep-9"), SessionUser::Root)
            .unwrap();
        assert!(known.exists());
        ssh.publish(&name("web"), Path::new("/run/ep-2"), SessionUser::Root)
            .unwrap();
        assert!(!known.exists());
        // A host that starts anew has no entries, so its first publish forgets the key.
        std::fs::write(&known, "puddle-web ssh-ed25519 AAAA key\n").unwrap();
        drop(ssh);
        let (again, _) = setup(dir.path(), false);
        again
            .publish(&name("web"), Path::new("/run/ep-2"), SessionUser::Root)
            .unwrap();
        assert!(!known.exists());
    }

    #[test]
    fn a_config_that_cannot_be_read_is_an_error_not_an_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("config");
        std::fs::create_dir(&user).unwrap();
        assert!(ensure_include(&user, &dir.path().join("p")).is_err());
    }
}
