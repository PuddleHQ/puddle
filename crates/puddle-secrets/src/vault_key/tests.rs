// SPDX-License-Identifier: GPL-3.0-or-later
//! The key sources that need no `systemd-creds`: the unit's credential, the key file, the
//! refusals. The sealing paths run against a fake tool in `tests/integration/vault_key.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]

use super::*;

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn other_platforms_have_no_key_source() {
    let dir = tempfile::tempdir().unwrap();
    let keys = VaultKeys::new(dir.path(), &dir.path().join("vault"), ToolPaths::default());
    assert_eq!(
        keys.find_or_make().await.unwrap_err(),
        KeyError::NoKeySource
    );
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    struct Setup {
        dir: tempfile::TempDir,
    }

    impl Setup {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn vault(&self) -> PathBuf {
            self.dir.path().join("data").join("secrets").join("vault")
        }

        fn config(&self) -> PathBuf {
            self.dir.path().join("config")
        }

        fn credentials(&self) -> PathBuf {
            self.dir.path().join("credentials")
        }

        /// No `systemd-creds` and no unit credential unless a test adds one.
        fn keys(&self) -> VaultKeys {
            VaultKeys::new(&self.config(), &self.vault(), ToolPaths::default())
                .with_credentials_dir(None)
        }

        fn keys_with_unit(&self) -> VaultKeys {
            self.keys().with_credentials_dir(Some(self.credentials()))
        }

        fn make_vault(&self, key: VaultKey, tag: KeySourceTag) {
            FileVault::create(&self.vault(), key, tag).unwrap();
        }

        fn write_unit_credential(&self, bytes: &[u8]) {
            fs::create_dir_all(self.credentials()).unwrap();
            fs::write(self.credentials().join(CREDENTIAL_NAME), bytes).unwrap();
        }

        fn key_file(&self) -> PathBuf {
            self.config().join("puddle").join(KEY_FILE)
        }
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[tokio::test]
    async fn the_first_start_without_systemd_makes_an_owner_only_key_file() {
        let setup = Setup::new();
        let found = setup.keys().find_or_make().await.unwrap();
        assert_eq!(found.source, KeySource::KeyFile);
        assert_eq!(found.tag, KeySourceTag::KeyFile);
        assert_eq!(mode_of(&setup.key_file()), 0o600);
        assert_eq!(mode_of(setup.key_file().parent().unwrap()), 0o700);
        // The file holds the key, and nothing else.
        assert_eq!(
            fs::read(setup.key_file()).unwrap(),
            found.key.expose().to_vec()
        );
    }

    #[tokio::test]
    async fn the_next_start_finds_the_same_key_and_opens_the_vault() {
        let setup = Setup::new();
        let first = setup.keys().find_or_make().await.unwrap();
        let vault = FileVault::create(&setup.vault(), first.key, first.tag).unwrap();
        drop(vault);
        let again = setup.keys().find_or_make().await.unwrap();
        assert_eq!(again.source, KeySource::KeyFile);
        assert_eq!(again.tag, KeySourceTag::KeyFile);
        FileVault::open(&setup.vault(), again.key).unwrap();
    }

    #[tokio::test]
    async fn a_key_file_others_can_read_is_refused() {
        let setup = Setup::new();
        setup.keys().find_or_make().await.unwrap();
        for mode in [0o644, 0o640, 0o604] {
            set_mode(&setup.key_file(), mode);
            let err = setup.keys().find_or_make().await.unwrap_err();
            let KeyError::KeyFileUnsafe(why) = err else {
                panic!("mode {mode:o}: {err:?}");
            };
            assert!(why.contains("0600"), "{why}");
        }
        set_mode(&setup.key_file(), 0o600);
        setup.keys().find_or_make().await.unwrap();
    }

    #[tokio::test]
    async fn a_key_file_of_the_wrong_length_is_refused() {
        for bytes in [
            &b""[..],
            &[7_u8; 31][..],
            &[7_u8; 33][..],
            &[7_u8; 4096][..],
        ] {
            let setup = Setup::new();
            puddle_fs::private::write_atomic(&setup.key_file(), bytes).unwrap();
            assert_eq!(
                setup.keys().find_or_make().await.unwrap_err(),
                KeyError::KeyFileInvalid,
                "{} bytes",
                bytes.len()
            );
        }
    }

    #[tokio::test]
    async fn the_unit_credential_wins_over_a_key_file() {
        let setup = Setup::new();
        puddle_fs::private::write_atomic(&setup.key_file(), &[1; 32]).unwrap();
        setup.write_unit_credential(&[2; 32]);
        let found = setup.keys_with_unit().find_or_make().await.unwrap();
        assert_eq!(found.source, KeySource::UnitCredential);
        assert_eq!(found.key.expose(), &[2; 32]);
        assert_eq!(found.tag, KeySourceTag::SystemdHost);
    }

    #[tokio::test]
    async fn a_unit_credential_of_the_wrong_length_is_refused() {
        for bytes in [&b""[..], &[2_u8; 31][..], &[2_u8; 33][..]] {
            let setup = Setup::new();
            setup.write_unit_credential(bytes);
            assert_eq!(
                setup.keys_with_unit().find_or_make().await.unwrap_err(),
                KeyError::UnitCredentialInvalid
            );
        }
    }

    #[tokio::test]
    async fn an_empty_credential_folder_and_a_relative_one_are_no_credential() {
        let setup = Setup::new();
        fs::create_dir_all(setup.credentials()).unwrap();
        let found = setup.keys_with_unit().find_or_make().await.unwrap();
        assert_eq!(found.source, KeySource::KeyFile);
        // A relative `$CREDENTIALS_DIRECTORY` would be read against the working folder.
        let setup = Setup::new();
        let relative = VaultKeys::new(&setup.config(), &setup.vault(), ToolPaths::default())
            .with_credentials_dir(Some(PathBuf::from("credentials")));
        assert_eq!(
            relative.find_or_make().await.unwrap().source,
            KeySource::KeyFile
        );
    }

    #[tokio::test]
    async fn a_vault_sealed_with_the_key_file_refuses_a_unit_credential() {
        let setup = Setup::new();
        setup.make_vault(VaultKey::from_bytes([1; 32]), KeySourceTag::KeyFile);
        puddle_fs::private::write_atomic(&setup.key_file(), &[1; 32]).unwrap();
        setup.write_unit_credential(&[2; 32]);
        assert_eq!(
            setup.keys_with_unit().find_or_make().await.unwrap_err(),
            KeyError::WrongSource {
                header: KeySourceTag::KeyFile,
                found: KeySource::UnitCredential
            }
        );
    }

    #[tokio::test]
    async fn a_systemd_vault_refuses_a_key_file() {
        for tag in [KeySourceTag::SystemdHost, KeySourceTag::SystemdHostTpm2] {
            let setup = Setup::new();
            setup.make_vault(VaultKey::from_bytes([1; 32]), tag);
            puddle_fs::private::write_atomic(&setup.key_file(), &[1; 32]).unwrap();
            let err = setup.keys().find_or_make().await.unwrap_err();
            assert_eq!(
                err,
                KeyError::WrongSource {
                    header: tag,
                    found: KeySource::KeyFile
                }
            );
            // The message names both sides and no key or path.
            assert!(err.to_string().contains("key-file"), "{err}");
        }
    }

    #[tokio::test]
    async fn a_systemd_vault_accepts_a_unit_credential_of_either_tag() {
        for tag in [KeySourceTag::SystemdHost, KeySourceTag::SystemdHostTpm2] {
            let setup = Setup::new();
            setup.make_vault(VaultKey::from_bytes([2; 32]), tag);
            setup.write_unit_credential(&[2; 32]);
            let found = setup.keys_with_unit().find_or_make().await.unwrap();
            assert_eq!((found.source, found.tag), (KeySource::UnitCredential, tag));
            FileVault::open(&setup.vault(), found.key).unwrap();
        }
    }

    #[tokio::test]
    async fn a_vault_with_no_key_anywhere_never_gets_a_new_one() {
        for tag in [KeySourceTag::KeyFile, KeySourceTag::SystemdHost] {
            let setup = Setup::new();
            setup.make_vault(VaultKey::from_bytes([1; 32]), tag);
            assert_eq!(
                setup.keys().find_or_make().await.unwrap_err(),
                KeyError::KeyMissing(tag)
            );
            assert!(!setup.key_file().exists(), "a key was made over a vault");
        }
    }

    #[tokio::test]
    async fn a_sealed_key_file_with_no_systemd_creds_says_so() {
        let setup = Setup::new();
        puddle_fs::private::write_atomic(&setup.config().join("puddle").join(SEALED_FILE), b"x")
            .unwrap();
        assert_eq!(
            setup.keys().find_or_make().await.unwrap_err(),
            KeyError::Sealed(Unseal::ToolMissing)
        );
    }

    #[tokio::test]
    async fn a_systemd_creds_that_cannot_start_is_told_apart_from_a_missing_one() {
        let setup = Setup::new();
        puddle_fs::private::write_atomic(&setup.config().join("puddle").join(SEALED_FILE), b"x")
            .unwrap();
        let keys_with = |tool: PathBuf| {
            VaultKeys::new(
                &setup.config(),
                &setup.vault(),
                ToolPaths::default().with_systemd_creds(Some(tool)),
            )
            .with_credentials_dir(None)
        };
        // A path with nothing at it: the tool is gone since it was looked up.
        assert_eq!(
            keys_with(setup.dir.path().join("gone"))
                .find_or_make()
                .await
                .unwrap_err(),
            KeyError::Sealed(Unseal::ToolMissing)
        );
        // A file nobody may run.
        let plain = setup.dir.path().join("plain");
        fs::write(&plain, b"").unwrap();
        set_mode(&plain, 0o644);
        assert_eq!(
            keys_with(plain).find_or_make().await.unwrap_err(),
            KeyError::Sealed(Unseal::Failed)
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_vault_is_not_overwritten_with_a_new_key() {
        let setup = Setup::new();
        fs::create_dir_all(setup.vault().parent().unwrap()).unwrap();
        fs::write(setup.vault(), b"not a vault at all").unwrap();
        assert_eq!(
            setup.keys().find_or_make().await.unwrap_err(),
            KeyError::Vault(VaultError::NotAVault)
        );
        assert!(!setup.key_file().exists());
    }
}

#[test]
fn the_major_version_comes_from_the_first_line() {
    assert_eq!(
        major_version(b"systemd 257 (257.13-1~deb13u1)\n+PAM +AUDIT\n"),
        Some(257)
    );
    assert_eq!(
        major_version(b"systemd 255 (255.4-1ubuntu8.18)\n"),
        Some(255)
    );
    assert_eq!(major_version(b""), None);
    assert_eq!(major_version(b"systemd\n"), None);
    assert_eq!(major_version(b"systemd next\n"), None);
    assert_eq!(major_version(&[0xff, 0xfe]), None);
}

#[test]
fn a_failed_decrypt_is_told_apart_by_the_measured_texts() {
    assert_eq!(
        classify(b"Failed to decrypt: io.systemd.System\n"),
        Unseal::Rejected
    );
    assert_eq!(
        classify(b"Name in credential doesn't match expectations.\n"),
        Unseal::NameMismatch
    );
    assert_eq!(
        classify(b"Failed to connect to the service\n"),
        Unseal::Failed
    );
    assert_eq!(classify(b""), Unseal::Failed);
}

#[test]
fn only_the_systemd_tags_fit_the_systemd_sources() {
    for source in [KeySource::UnitCredential, KeySource::SystemdSealed] {
        assert!(source.fits(KeySourceTag::SystemdHost));
        assert!(source.fits(KeySourceTag::SystemdHostTpm2));
        assert!(!source.fits(KeySourceTag::KeyFile));
    }
    assert!(KeySource::KeyFile.fits(KeySourceTag::KeyFile));
    assert!(!KeySource::KeyFile.fits(KeySourceTag::SystemdHost));
    assert!(!KeySource::KeyFile.fits(KeySourceTag::SystemdHostTpm2));
}

#[test]
fn the_names_are_stable() {
    assert_eq!(KeySource::UnitCredential.as_str(), "unit-credential");
    assert_eq!(KeySource::SystemdSealed.as_str(), "systemd-sealed");
    assert_eq!(KeySource::KeyFile.as_str(), "key-file");
}

#[test]
fn the_config_home_is_an_absolute_xdg_folder_or_dot_config_under_home() {
    let os = |s: &str| Some(OsString::from(s));
    let abs = |p: &str| Path::new(if cfg!(windows) { "C:" } else { "" }).join(p);
    let (xdg, home) = (abs("/xdg"), abs("/home/me"));
    assert_eq!(
        config_home_from(os(xdg.to_str().unwrap()), os(home.to_str().unwrap())),
        Some(xdg)
    );
    assert_eq!(
        config_home_from(os("relative"), os(home.to_str().unwrap())),
        Some(home.join(".config"))
    );
    assert_eq!(
        config_home_from(None, os(home.to_str().unwrap())),
        Some(home.join(".config"))
    );
    assert_eq!(config_home_from(None, os("home")), None);
    assert_eq!(config_home_from(None, None), None);
}

#[test]
fn the_config_home_reads_the_environment() {
    assert_eq!(
        config_home(),
        config_home_from(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME")
        )
    );
}
