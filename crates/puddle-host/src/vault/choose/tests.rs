// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_secrets::{KeySourceTag, MemoryStore, StoreHealth, StoredId, read_record, write_record};
#[cfg(target_os = "linux")]
use puddle_secrets::{Secret, vault_path};
use puddle_store::{EnvDraft, EnvName, EnvScope, Limits, ManualClock, SecretHost};

use super::*;

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

struct Rig {
    dir: tempfile::TempDir,
    store: Store,
    problems: Problems,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            store: Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap(),
            problems: Problems::new(Arc::new(puddle_types::NullSink)),
        }
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn key_home(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    #[cfg(target_os = "linux")]
    fn key_file(&self) -> PathBuf {
        self.key_home().join("puddle").join("vault.key")
    }

    /// A setup that reads nothing of this machine: no `systemd-creds` (so the key is a file in the
    /// rig's config folder) and a session the test names.
    fn setup(&self, session: LoginSession) -> SecretSetup {
        SecretSetup {
            session: Some(session),
            key_home: Some(self.key_home()),
            tools: Some(ToolPaths::default()),
            ..SecretSetup::default()
        }
    }

    async fn open(&self, setup: &SecretSetup) -> Result<Chosen, HostError> {
        std::fs::create_dir_all(self.data()).unwrap();
        open(
            None,
            setup,
            &HostPaths::new(self.data()),
            &self.store,
            &self.problems,
        )
        .await
    }

    fn name_a_secret(&self) {
        self.store
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

    fn recorded(&self) -> Option<(StoreKind, String)> {
        read_record(&self.data()).unwrap().map(|r| (r.store, r.why))
    }
}

fn working_os() -> Arc<MemoryStore> {
    Arc::new(MemoryStore::new())
}

fn with_os(mut setup: SecretSetup, os: &Arc<MemoryStore>) -> SecretSetup {
    setup.os_store = Some(os.clone());
    setup
}

fn headless(mut setup: SecretSetup) -> SecretSetup {
    setup.headless = true;
    setup
}

fn asking(mut setup: SecretSetup, option: StoreOption) -> SecretSetup {
    setup.option = option;
    setup
}

#[tokio::test]
async fn a_store_the_host_was_handed_is_used_as_it_is_and_nothing_is_decided() {
    let rig = Rig::new();
    let given: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let chosen = open(
        Some(&given),
        &rig.setup(LoginSession::Unknown),
        &HostPaths::new(rig.data()),
        &rig.store,
        &rig.problems,
    )
    .await
    .unwrap();
    assert!(chosen.usable);
    assert!(Arc::ptr_eq(&chosen.vault, &given));
    assert!(!rig.data().exists(), "no folder, no record");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_headless_first_start_keeps_secrets_in_a_file_vault_and_a_later_start_reads_them_back() {
    let rig = Rig::new();
    let setup = headless(rig.setup(LoginSession::NotGraphical));
    let chosen = rig.open(&setup).await.unwrap();
    assert!(chosen.usable);
    let canary = "PUDDLE-CANARY-4f9c1e";
    chosen
        .vault
        .set(&id("tok-a"), &Secret::new(canary.to_owned()))
        .unwrap();
    assert_eq!(
        rig.recorded(),
        Some((StoreKind::File, Rule::Headless.why().to_owned()))
    );
    assert_eq!(
        FileVault::header_source(&vault_path(&rig.data())).unwrap(),
        Some(KeySourceTag::KeyFile),
        "no systemd-creds in this rig, so the key is a file"
    );
    let bytes = std::fs::read(vault_path(&rig.data())).unwrap();
    assert!(
        !bytes.windows(canary.len()).any(|w| w == canary.as_bytes()),
        "the plain value is nowhere in the vault file"
    );
    drop(chosen);

    // The next start has no flag at all and still finds the vault and the value.
    let again = rig.open(&rig.setup(LoginSession::Graphical)).await.unwrap();
    let got = again.vault.get(&id("tok-a")).unwrap().unwrap();
    assert_eq!(got.expose(), canary);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_recorded_store_is_never_switched_by_the_flag_or_by_a_keyring_that_now_works() {
    // A recorded file vault stays, with a working keyring and no flag.
    let rig = Rig::new();
    rig.open(&headless(rig.setup(LoginSession::Unknown)))
        .await
        .unwrap();
    let os = working_os();
    let again = rig
        .open(&with_os(rig.setup(LoginSession::Graphical), &os))
        .await
        .unwrap();
    assert!(matches!(again.probe, Probe::File(_)));
    assert_eq!(rig.recorded().unwrap().0, StoreKind::File);

    // A recorded os stays, with the flag and a failing keyring; the problem is the probe's.
    let rig = Rig::new();
    let os = working_os();
    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    os.break_it();
    let chosen = rig
        .open(&with_os(headless(rig.setup(LoginSession::Unknown)), &os))
        .await
        .unwrap();
    assert!(matches!(chosen.probe, Probe::Store(_)));
    assert_eq!(rig.recorded().unwrap().0, StoreKind::Os);
    let problems = Arc::new(Problems::new(Arc::new(puddle_types::NullSink)));
    chosen.check_in_background(problems.clone()).await.unwrap();
    let raised = problems.list();
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert_eq!(raised[0].key, STORE_PROBLEM);
    assert_eq!(rig.recorded().unwrap().0, StoreKind::Os, "still os");
}

#[tokio::test]
async fn an_option_that_disagrees_with_the_record_stops_the_start_with_the_two_stores_named() {
    let rig = Rig::new();
    write_record(&rig.data(), StoreKind::Os, Rule::Option).unwrap();
    let err = rig
        .open(&asking(rig.setup(LoginSession::Unknown), StoreOption::File))
        .await
        .err()
        .unwrap();
    let text = err.to_string();
    assert!(
        text.contains("the operating system's credential store"),
        "{text}"
    );
    assert!(text.contains("Puddle's file vault"), "{text}");
    assert!(
        !text.contains(rig.dir.path().to_str().unwrap()),
        "no path: {text}"
    );
}

#[tokio::test]
async fn an_unreadable_record_stops_the_start_instead_of_choosing_again() {
    let rig = Rig::new();
    let folder = puddle_secrets::secrets_folder(&rig.data());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("store.json"), "{ not json").unwrap();
    let err = rig
        .open(&headless(rig.setup(LoginSession::Unknown)))
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("store.json"), "{err}");
}

#[tokio::test]
async fn an_install_whose_rows_name_a_secret_records_os_even_headless_and_with_a_failing_probe() {
    let rig = Rig::new();
    rig.name_a_secret();
    let os = working_os();
    os.break_it();
    let chosen = rig
        .open(&with_os(
            headless(rig.setup(LoginSession::NotGraphical)),
            &os,
        ))
        .await
        .unwrap();
    assert!(matches!(chosen.probe, Probe::Store(_)));
    assert_eq!(
        rig.recorded(),
        Some((StoreKind::Os, Rule::ExistingSecrets.why().to_owned())),
        "recorded at once, whatever the keyring says"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn an_install_whose_rows_name_no_secret_follows_the_headless_rule() {
    let rig = Rig::new();
    let chosen = rig
        .open(&headless(rig.setup(LoginSession::NotGraphical)))
        .await
        .unwrap();
    assert!(matches!(chosen.probe, Probe::File(_)));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_start_without_the_flag_records_os_only_once_the_keyring_takes_a_write() {
    let rig = Rig::new();
    let os = working_os();
    os.break_it();
    let setup = with_os(rig.setup(LoginSession::NotGraphical), &os);
    let chosen = rig.open(&setup).await.unwrap();
    assert!(chosen.warn_without_flag.is_some());
    let problems = Arc::new(Problems::new(Arc::new(puddle_types::NullSink)));
    chosen.check_in_background(problems.clone()).await.unwrap();
    assert_eq!(problems.list().len(), 1);
    assert_eq!(
        rig.recorded(),
        None,
        "a keyring that stored nothing decides nothing"
    );

    // So a later start with the flag may still pick the vault.
    let later = rig
        .open(&headless(rig.setup(LoginSession::NotGraphical)))
        .await
        .unwrap();
    assert!(matches!(later.probe, Probe::File(_)));
    assert_eq!(rig.recorded().unwrap().0, StoreKind::File);

    // A working keyring is recorded after its test write.
    let rig = Rig::new();
    let os = working_os();
    let chosen = rig
        .open(&with_os(rig.setup(LoginSession::Graphical), &os))
        .await
        .unwrap();
    assert_eq!(rig.recorded(), None, "not before the test write");
    chosen
        .check_in_background(Arc::new(Problems::new(Arc::new(puddle_types::NullSink))))
        .await
        .unwrap();
    assert_eq!(
        rig.recorded(),
        Some((StoreKind::Os, Rule::LinuxDefault.why().to_owned()))
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn only_a_start_that_took_the_default_without_the_flag_is_checked_for_the_warning() {
    let rig = Rig::new();
    let os = working_os();
    let session = LoginSession::NotGraphical;
    let plain = rig.open(&with_os(rig.setup(session), &os)).await.unwrap();
    assert_eq!(plain.warn_without_flag, Some(session));

    let rig = Rig::new();
    let asked = rig
        .open(&asking(with_os(rig.setup(session), &os), StoreOption::Os))
        .await
        .unwrap();
    assert_eq!(
        asked.warn_without_flag, None,
        "an explicit option is no mistake"
    );

    let rig = Rig::new();
    let flagged = rig
        .open(&headless(with_os(rig.setup(session), &os)))
        .await
        .unwrap();
    assert_eq!(flagged.warn_without_flag, None);
}

#[test]
fn the_warning_is_for_a_text_login_or_a_store_that_does_not_work() {
    use crate::vault::warns;
    let rows = [
        (LoginSession::NotGraphical, StoreHealth::Working, true),
        (LoginSession::Graphical, StoreHealth::Locked, true),
        (LoginSession::Unknown, StoreHealth::NoProvider, true),
        (LoginSession::Graphical, StoreHealth::Working, false),
        (LoginSession::Unknown, StoreHealth::Working, false),
    ];
    for (session, health, want) in rows {
        assert_eq!(warns(session, health), want, "{session:?} {health:?}");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_vault_whose_key_is_missing_leaves_a_running_host_a_refusing_store_and_one_problem() {
    let rig = Rig::new();
    rig.open(&headless(rig.setup(LoginSession::Unknown)))
        .await
        .unwrap();
    std::fs::remove_file(rig.key_file()).unwrap();
    let before = std::fs::read(vault_path(&rig.data())).unwrap();

    let chosen = rig.open(&rig.setup(LoginSession::Unknown)).await.unwrap();
    assert!(!chosen.usable);
    assert!(
        chosen
            .vault
            .set(&id("tok-a"), &Secret::new("x".into()))
            .is_err()
    );
    assert!(chosen.vault.get(&id("tok-a")).is_err());
    let raised = rig.problems.list();
    assert_eq!(raised.len(), 1, "{raised:?}");
    assert_eq!(raised[0].key, STORE_PROBLEM);
    assert!(
        raised[0].title.contains("its key is missing"),
        "{}",
        raised[0].title
    );
    assert!(
        raised[0].detail.contains("key-file key"),
        "{}",
        raised[0].detail
    );
    assert_eq!(
        std::fs::read(vault_path(&rig.data())).unwrap(),
        before,
        "no new key, no new vault over the old one"
    );
    assert!(!rig.key_file().exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_key_file_that_others_can_read_is_a_problem_that_says_chmod() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    rig.open(&headless(rig.setup(LoginSession::Unknown)))
        .await
        .unwrap();
    std::fs::set_permissions(rig.key_file(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let chosen = rig.open(&rig.setup(LoginSession::Unknown)).await.unwrap();
    assert!(!chosen.usable);
    let raised = rig.problems.list();
    assert!(
        raised[0].title.contains("can be read by others"),
        "{raised:?}"
    );
    assert!(raised[0].detail.contains("chmod 600"), "{raised:?}");
}

#[test]
fn every_way_a_key_can_fail_gives_a_problem_with_a_title_and_a_fix_and_no_key_or_path() {
    use puddle_secrets::Unseal;
    let causes = [
        KeyError::NoKeySource,
        KeyError::WrongSource {
            header: KeySourceTag::KeyFile,
            found: puddle_secrets::KeySource::SystemdSealed,
        },
        KeyError::KeyMissing(KeySourceTag::SystemdHost),
        KeyError::UnitCredentialInvalid,
        KeyError::KeyFileUnsafe("mode 644".into()),
        KeyError::KeyFileInvalid,
        KeyError::Sealed(Unseal::Rejected),
        KeyError::Sealed(Unseal::ToolMissing),
        KeyError::Sealed(Unseal::Unsupported),
        KeyError::Sealed(Unseal::NameMismatch),
        KeyError::Sealed(Unseal::Failed),
        KeyError::Sealed(Unseal::TimedOut),
        KeyError::Sealed(Unseal::NotAKey),
        KeyError::Vault(VaultError::WrongKey),
        KeyError::Vault(VaultError::Corrupt),
        KeyError::Io,
        KeyError::Random,
    ];
    for cause in causes {
        let failure = key_failure(&cause);
        assert!(failure.title.starts_with("Puddle cannot"), "{cause:?}");
        assert!(failure.fix.len() > 40, "{cause:?}: {}", failure.fix);
        assert!(!failure.fix.contains("/home"), "{cause:?}");
        assert_ne!(failure.cause, "");
    }
    for cause in [
        VaultError::Io,
        VaultError::Missing,
        VaultError::Exists,
        VaultError::NotAVault,
        VaultError::UnknownVersion(9),
        VaultError::WrongKey,
        VaultError::Corrupt,
        VaultError::Random,
    ] {
        let failure = vault_failure(cause);
        assert!(failure.fix.len() > 40, "{cause:?}");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_file_vault_check_clears_after_a_working_write_and_never_probes_the_keyring() {
    let rig = Rig::new();
    let chosen = rig
        .open(&headless(rig.setup(LoginSession::Unknown)))
        .await
        .unwrap();
    let problems = Arc::new(Problems::new(Arc::new(puddle_types::NullSink)));
    chosen.check_in_background(problems.clone()).await.unwrap();
    assert_eq!(problems.list(), Vec::new());
}

#[cfg(not(target_os = "linux"))]
#[tokio::test]
async fn asking_for_the_file_vault_where_there_is_no_key_source_is_a_problem_and_records_nothing() {
    let rig = Rig::new();
    let chosen = rig
        .open(&asking(rig.setup(LoginSession::Unknown), StoreOption::File))
        .await
        .unwrap();
    assert!(!chosen.usable);
    assert_eq!(rig.recorded(), None);
}
