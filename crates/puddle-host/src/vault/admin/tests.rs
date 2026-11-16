// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_secrets::{MemoryStore, Secret};
use puddle_store::{EnvDraft, EnvName, EnvScope, ManualClock, SecretHost};
use puddle_types::WorkspaceId;

use super::*;
use crate::ProxyPasswords;

const CANARY: &str = "PUDDLE-CANARY-7d21aa";

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

struct Rig {
    dir: tempfile::TempDir,
    os: Arc<MemoryStore>,
}

impl Rig {
    fn new() -> Self {
        let rig = Self {
            dir: tempfile::tempdir().unwrap(),
            os: Arc::new(MemoryStore::new()),
        };
        std::fs::create_dir_all(rig.data()).unwrap();
        rig
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn paths(&self) -> HostPaths {
        HostPaths::new(self.data())
    }

    fn setup(&self) -> SecretSetup {
        SecretSetup {
            key_home: Some(self.dir.path().join("config")),
            tools: Some(ToolPaths::default()),
            os_store: Some(self.os.clone()),
            ..SecretSetup::default()
        }
    }

    /// The rows: one variable named `TOKEN` whose value is the secret `env-named`.
    fn name_a_secret(&self) {
        let store = Store::open(
            &self.paths().store(),
            Arc::new(ManualClock::new(1)),
            Limits::default(),
        )
        .unwrap();
        puddle_store::testing::own(&store, &WorkspaceId::new("a").unwrap());
        store
            .set_env(
                puddle_store::UserId::OWNER,
                &EnvScope::Global,
                &EnvName::new("TOKEN").unwrap(),
                EnvDraft::secret(
                    id("env-named"),
                    vec![SecretHost::new("a.example.com").unwrap()],
                )
                .unwrap(),
            )
            .unwrap();
    }

    /// What an older install left: the secret and a proxy sign-in in the OS store.
    fn fill_os(&self) {
        self.name_a_secret();
        self.os
            .set(&id("env-named"), &Secret::new(CANARY.to_owned()))
            .unwrap();
        let passwords = ProxyPasswords::new(self.os.clone());
        passwords
            .set(
                &puddle_upstream::ProxyAddr::new("proxy.corp", 8080),
                "alice",
                &puddle_types::fixture::password("vault"),
            )
            .unwrap();
    }

    fn recorded(&self) -> Option<(StoreKind, String)> {
        read_record(self.data().as_path())
            .unwrap()
            .map(|r| (r.store, r.why))
    }

    #[cfg(target_os = "linux")]
    fn vault(&self) -> Arc<dyn SecretStore> {
        file_store(&self.paths(), &self.setup(), false)
            .unwrap()
            .expect("a vault")
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_move_to_the_file_vault_and_back_carries_the_rows_secret_and_the_proxy_sign_in() {
    let rig = Rig::new();
    rig.fill_os();
    // No record: what every earlier install used is the operating system's store.
    assert!(rig.recorded().is_none());

    let there = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File).unwrap();
    assert_eq!(
        there,
        MigrateOutcome::Moved { copied: 3, left: 0 },
        "the secret, the proxy list and the proxy entry"
    );
    assert_eq!(
        rig.recorded(),
        Some((StoreKind::File, Rule::Migrated.why().to_owned()))
    );
    let vault = rig.vault();
    assert_eq!(
        vault.get(&id("env-named")).unwrap().unwrap().expose(),
        CANARY
    );
    assert_eq!(vault.listed_ids().unwrap().len(), 3, "the vault's index");
    assert_eq!(ProxyPasswords::new(vault).load().unwrap(), 1);
    assert!(rig.os.get(&id("env-named")).unwrap().is_none());
    assert_eq!(
        rig.os.ids(),
        Vec::<String>::new(),
        "nothing is left in the old store"
    );

    let back = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::Os).unwrap();
    assert_eq!(back, MigrateOutcome::Moved { copied: 3, left: 0 });
    assert_eq!(rig.recorded().unwrap().0, StoreKind::Os);
    assert_eq!(
        rig.os.get(&id("env-named")).unwrap().unwrap().expose(),
        CANARY
    );
    assert_eq!(ProxyPasswords::new(rig.os.clone()).load().unwrap(), 1);
    assert!(rig.vault().get(&id("env-named")).unwrap().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn a_rerun_removes_what_a_crash_after_the_switch_left_in_the_old_store() {
    let rig = Rig::new();
    rig.fill_os();
    vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File).unwrap();
    // The crash: the record says file, and the old store still holds a stale copy. The store in
    // charge has moved on since.
    rig.os
        .set(&id("env-named"), &Secret::new("stale".to_owned()))
        .unwrap();
    rig.vault()
        .set(&id("env-named"), &Secret::new("newer".to_owned()))
        .unwrap();

    let again = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File).unwrap();
    assert_eq!(
        again,
        MigrateOutcome::Finished {
            removed: 1,
            left: 0
        }
    );
    assert_eq!(rig.os.ids(), Vec::<String>::new());
    assert_eq!(
        rig.vault().get(&id("env-named")).unwrap().unwrap().expose(),
        "newer",
        "the store in charge wins"
    );
    let third = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File).unwrap();
    assert_eq!(
        third,
        MigrateOutcome::Finished {
            removed: 0,
            left: 0
        }
    );
}

#[test]
fn a_folder_that_already_keeps_its_secrets_there_is_left_alone() {
    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    rig.os
        .set(&id("env-named"), &Secret::new(CANARY.to_owned()))
        .unwrap();
    let outcome = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::Os).unwrap();
    assert_eq!(outcome, MigrateOutcome::AlreadyThere(StoreKind::Os));
    assert_eq!(rig.os.ids(), vec!["env-named".to_owned()]);
}

#[test]
fn a_folder_with_no_record_just_records_os() {
    let rig = Rig::new();
    let outcome = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::Os).unwrap();
    assert_eq!(outcome, MigrateOutcome::Recorded);
    assert_eq!(
        rig.recorded(),
        Some((StoreKind::Os, Rule::Migrated.why().to_owned()))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_store_that_cannot_be_read_stops_migrate_and_changes_nothing() {
    let rig = Rig::new();
    rig.fill_os();
    rig.os.break_it();
    let err = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File).unwrap_err();
    assert!(
        matches!(err, VaultAdminError::Move(MigrateError::Source)),
        "{err}"
    );
    assert!(rig.recorded().is_none(), "no record was written");
    rig.os.heal();
    assert_eq!(
        rig.os.get(&id("env-named")).unwrap().unwrap().expose(),
        CANARY
    );
}

#[test]
fn a_running_host_makes_migrate_and_reset_refuse_and_name_itself() {
    let rig = Rig::new();
    let _host = DataLock::acquire(&rig.data()).unwrap();
    for text in [
        vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File)
            .unwrap_err()
            .to_string(),
        vault_reset(&rig.paths(), &rig.setup(), true)
            .unwrap_err()
            .to_string(),
    ] {
        assert!(
            text.contains(&format!("process ID {}", std::process::id())),
            "{text}"
        );
        assert!(text.contains("already using the data folder"), "{text}");
    }
    assert!(rig.recorded().is_none());
    // Status only reads and works beside a host.
    assert_eq!(
        vault_status(&rig.paths(), &rig.setup()).unwrap().state,
        StoreState::Undecided
    );
}

#[test]
fn status_says_nothing_is_chosen_before_a_start_decided() {
    let rig = Rig::new();
    rig.name_a_secret();
    let status = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(
        status,
        VaultStatus {
            record: None,
            named: 1,
            key_source: None,
            state: StoreState::Undecided,
        }
    );
}

#[test]
fn status_counts_the_named_secrets_the_os_store_holds() {
    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    rig.name_a_secret();
    let missing = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(missing.named, 1);
    assert_eq!(missing.state, StoreState::Readable { found: 0 });
    assert_eq!(missing.record.unwrap().store, StoreKind::Os);
    rig.os
        .set(&id("env-named"), &Secret::new(CANARY.to_owned()))
        .unwrap();
    let found = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(found.state, StoreState::Readable { found: 1 });
}

#[test]
fn a_locked_os_store_is_a_problem_in_status_not_a_hang_or_an_error() {
    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    rig.name_a_secret();
    rig.os.break_it();
    let status = vault_status(&rig.paths(), &rig.setup()).unwrap();
    let StoreState::Unreadable { title, fix } = status.state else {
        panic!("{:?}", status.state);
    };
    assert!(title.contains("could not be read"), "{title}");
    assert!(fix.contains("unlock"), "{fix}");
}

/// A store whose reads wait for the test to let go: a keyring waiting for the user to unlock it.
struct Blocked(std::sync::Mutex<mpsc::Receiver<()>>);

impl SecretStore for Blocked {
    fn get(&self, _: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
        // Returns when the test drops its end.
        let _ = self.0.lock().unwrap().recv();
        Ok(None)
    }
    fn set(&self, _: &StoredId, _: &Secret) -> Result<(), puddle_secrets::StoreError> {
        Ok(())
    }
    fn delete(&self, _: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        Ok(())
    }
}

#[test]
fn a_store_that_waits_for_an_unlock_is_given_the_wait_and_then_named() {
    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    rig.name_a_secret();
    let (release, waiting) = mpsc::channel();
    let mut setup = rig.setup();
    setup.os_store = Some(Arc::new(Blocked(std::sync::Mutex::new(waiting))));
    let status = status_within(&rig.paths(), &setup, Duration::from_millis(50)).unwrap();
    drop(release);
    let StoreState::Unreadable { title, fix } = status.state else {
        panic!("{:?}", status.state);
    };
    assert!(title.contains("did not answer within 0 seconds"), "{title}");
    assert!(fix.contains("unlock"), "{fix}");
    // A store that answers is read well inside the wait.
    setup.os_store = Some(rig.os.clone());
    let status = status_within(&rig.paths(), &setup, Duration::from_secs(60)).unwrap();
    assert_eq!(status.state, StoreState::Readable { found: 0 });
}

/// A file vault the way a headless first start makes it, with `CANARY` under `env-named`.
#[cfg(target_os = "linux")]
fn start_a_file_vault(rig: &Rig) {
    rig.name_a_secret();
    write_record(&rig.data(), StoreKind::File, Rule::Headless).unwrap();
    let vault = file_store(&rig.paths(), &rig.setup(), true)
        .unwrap()
        .unwrap();
    vault
        .set(&id("env-named"), &Secret::new(CANARY.to_owned()))
        .unwrap();
}

#[cfg(target_os = "linux")]
fn key_file(rig: &Rig) -> PathBuf {
    rig.dir
        .path()
        .join("config")
        .join("puddle")
        .join("vault.key")
}

/// Another key than the vault was made with, owner-only like the real one.
#[cfg(target_os = "linux")]
fn swap_the_key(rig: &Rig) {
    use std::os::unix::fs::PermissionsExt as _;
    let path = key_file(rig);
    std::fs::write(&path, [0x42_u8; 32]).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn status_names_the_file_vaults_key_source_and_reads_it() {
    let rig = Rig::new();
    start_a_file_vault(&rig);
    let status = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(status.key_source, Some(KeySourceTag::KeyFile));
    assert_eq!(status.state, StoreState::Readable { found: 1 });
    assert_eq!(status.record.unwrap().store, StoreKind::File);

    swap_the_key(&rig);
    let wrong = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(wrong.key_source, Some(KeySourceTag::KeyFile));
    let StoreState::Unreadable { title, .. } = wrong.state else {
        panic!("{:?}", wrong.state);
    };
    assert!(title.contains("file vault"), "{title}");
}

#[cfg(target_os = "linux")]
#[test]
fn status_for_a_recorded_file_vault_that_was_never_made_says_so() {
    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::File, Rule::Headless).unwrap();
    let status = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(status.state, StoreState::NoVault);
    assert!(!vault_path(&rig.data()).exists(), "status makes nothing");
    assert!(!key_file(&rig).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn reset_keeps_the_unreadable_vault_aside_starts_a_new_one_and_leaves_the_rows() {
    let rig = Rig::new();
    start_a_file_vault(&rig);
    let vault_file = vault_path(&rig.data());
    let old_vault = std::fs::read(&vault_file).unwrap();
    swap_the_key(&rig);

    // Without --yes: what would happen, and nothing happens.
    assert_eq!(
        vault_reset(&rig.paths(), &rig.setup(), false).unwrap(),
        ResetOutcome::Plan { key_files: 1 }
    );
    assert_eq!(std::fs::read(&vault_file).unwrap(), old_vault);
    assert_eq!(std::fs::read(key_file(&rig)).unwrap(), [0x42_u8; 32]);

    assert_eq!(
        vault_reset(&rig.paths(), &rig.setup(), true).unwrap(),
        ResetOutcome::Done { key_files: 1 }
    );
    let aside = |folder: &std::path::Path, prefix: &str| -> PathBuf {
        let mut found: Vec<PathBuf> = std::fs::read_dir(folder)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("{prefix}.unreadable-"))
            })
            .collect();
        assert_eq!(found.len(), 1, "{prefix}");
        found.remove(0)
    };
    let secrets = vault_file.parent().unwrap().to_path_buf();
    assert_eq!(
        std::fs::read(aside(&secrets, "vault")).unwrap(),
        old_vault,
        "the old vault is kept byte for byte"
    );
    let config = key_file(&rig).parent().unwrap().to_path_buf();
    assert_eq!(
        std::fs::read(aside(&config, "vault.key")).unwrap(),
        [0x42_u8; 32],
        "the old key is kept"
    );

    // A new vault with a new key opens and is empty; the record still says file.
    let status = vault_status(&rig.paths(), &rig.setup()).unwrap();
    assert_eq!(status.state, StoreState::Readable { found: 0 });
    assert_eq!(status.record.unwrap().store, StoreKind::File);
    assert_ne!(std::fs::read(key_file(&rig)).unwrap(), [0x42_u8; 32]);
    // The row still names the secret, so the host finds it missing.
    let store = Store::open(
        &rig.paths().store(),
        Arc::new(ManualClock::new(1)),
        Limits::default(),
    )
    .unwrap();
    let vault = rig.vault();
    let err = crate::environment::resolve(&store, vault.as_ref(), &WorkspaceId::new("a").unwrap())
        .expect_err("the secret is missing");
    assert!(matches!(
        err,
        crate::environment::EnvironmentError::SecretMissing { .. }
    ));
    // A second reset has nothing to do: the vault opens.
    assert!(matches!(
        vault_reset(&rig.paths(), &rig.setup(), true),
        Err(VaultAdminError::VaultOpens)
    ));
}

#[test]
fn reset_refuses_an_os_store_an_undecided_folder_a_missing_vault_and_a_vault_that_opens() {
    let rig = Rig::new();
    let refused = |rig: &Rig| vault_reset(&rig.paths(), &rig.setup(), true).unwrap_err();
    assert!(matches!(refused(&rig), VaultAdminError::Undecided));

    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    assert!(matches!(refused(&rig), VaultAdminError::NotAFileVault));

    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::File, Rule::Headless).unwrap();
    assert!(matches!(refused(&rig), VaultAdminError::NoVault));

    #[cfg(target_os = "linux")]
    {
        let rig = Rig::new();
        start_a_file_vault(&rig);
        let before = std::fs::read(vault_path(&rig.data())).unwrap();
        assert!(matches!(refused(&rig), VaultAdminError::VaultOpens));
        assert_eq!(std::fs::read(vault_path(&rig.data())).unwrap(), before);
    }
}

#[test]
fn the_error_and_status_texts_never_hold_a_value_or_an_id() {
    let rig = Rig::new();
    rig.fill_os();
    rig.os.break_it();
    let err = vault_migrate(&rig.paths(), &rig.setup(), StoreKind::File);
    let status = vault_status(&rig.paths(), &rig.setup());
    for text in [format!("{err:?}"), format!("{status:?}")] {
        assert!(!text.contains(CANARY), "{text}");
        assert!(!text.contains("env-named"), "{text}");
        assert!(!text.contains("proxy-"), "{text}");
    }
}
