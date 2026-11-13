// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]
//! The vault key sources that call `systemd-creds`, against a fake one that answers with the exits
//! and texts measured on real machines (systemd 255, 257, 258, 259), and against the real one where
//! this computer has systemd 256 or later.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_secrets::KeySourceTag;
use puddle_secrets::{
    FileVault, KeyError, KeySource, Secret, SecretStore, StoredId, ToolPaths, Unseal, VaultKeys,
};

use crate::common::Fakes;

/// The fake answers like systemd 257 with no TPM unless a test says otherwise; `encrypt` and
/// `decrypt` hand the key straight back, so a sealed file holds the key and a round trip works.
const HEALTHY: &str = "[--version]\nstdout=systemd 257 (257.13-1~deb13u1)\\n\n\
                       [encrypt]\necho_stdin=1\n\
                       [decrypt]\necho_stdin=1\n\
                       [has-tpm2]\nexit=19\n";

struct Setup {
    fakes: Fakes,
    dir: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        Self {
            fakes: Fakes::new(),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn config(&self) -> PathBuf {
        self.dir.path().join("config")
    }

    fn vault(&self) -> PathBuf {
        self.dir.path().join("data").join("secrets").join("vault")
    }

    fn sealed(&self) -> PathBuf {
        self.config().join("puddle").join("vault-key.cred")
    }

    fn key_file(&self) -> PathBuf {
        self.config().join("puddle").join("vault.key")
    }

    /// Keys over a fake `systemd-creds` with `behaviour`; also returns the fake's path for its log.
    fn keys(&self, behaviour: &str) -> (VaultKeys, PathBuf) {
        let existing = self.fakes.dir.path().join("systemd-creds");
        let tool = if existing.exists() {
            // A second start: the tool may be running or just ran, so only its behaviour changes.
            std::fs::write(format!("{}.behaviour", existing.display()), behaviour).unwrap();
            let _ = std::fs::remove_file(format!("{}.log", existing.display()));
            existing
        } else {
            self.fakes.install("systemd-creds", behaviour)
        };
        let paths = ToolPaths::default().with_systemd_creds(Some(tool.clone()));
        let keys = VaultKeys::new(&self.config(), &self.vault(), paths)
            .with_credentials_dir(None)
            .with_timeout(Duration::from_millis(500));
        (keys, tool)
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn calls(log: &str) -> Vec<&str> {
    log.lines()
        .filter_map(|l| l.strip_prefix("ARGS "))
        .collect()
}

#[tokio::test]
async fn the_first_start_seals_the_key_through_stdin_and_stdout() {
    let setup = Setup::new();
    let (keys, tool) = setup.keys(HEALTHY);
    let found = keys.find_or_make().await.unwrap();
    assert_eq!(found.source, KeySource::SystemdSealed);
    assert_eq!(found.tag, KeySourceTag::SystemdHost);
    // The file is what `encrypt` printed (the fake echoes the key), owner-only, and no key file exists.
    assert_eq!(std::fs::read(setup.sealed()).unwrap(), found.key.expose());
    assert_eq!(mode_of(&setup.sealed()), 0o600);
    assert!(!setup.key_file().exists());
    // Fixed arguments, the name sealed in, standard input and output only: no file argument.
    let log = Fakes::log(&tool);
    let all = calls(&log);
    assert!(
        all.contains(&"--user encrypt --name=puddle-vault-key - -"),
        "{all:?}"
    );
    assert!(
        all.contains(&"--user decrypt --name=puddle-vault-key - -"),
        "{all:?}"
    );
    assert!(
        all.contains(&"--version") && all.contains(&"has-tpm2"),
        "{all:?}"
    );
    assert!(!log.contains("GH_TOKEN=x"));
}

#[tokio::test]
async fn the_next_start_opens_the_sealed_key_and_the_vault() {
    let setup = Setup::new();
    let (keys, _) = setup.keys(HEALTHY);
    let first = keys.find_or_make().await.unwrap();
    let vault = FileVault::create(&setup.vault(), first.key, first.tag).unwrap();
    let id = StoredId::new("login-1").unwrap();
    vault.set(&id, &Secret::new("hunter2".to_owned())).unwrap();
    drop(vault);

    let (keys, tool) = setup.keys(HEALTHY);
    let again = keys.find_or_make().await.unwrap();
    assert_eq!(
        (again.source, again.tag),
        (KeySource::SystemdSealed, KeySourceTag::SystemdHost)
    );
    let vault = FileVault::open(&setup.vault(), again.key).unwrap();
    assert_eq!(vault.get(&id).unwrap().unwrap().expose(), "hunter2");
    // An existing vault names its tag, so nothing asks the machine about its TPM again and nothing is sealed again.
    let log = Fakes::log(&tool);
    assert_eq!(calls(&log), ["--user decrypt --name=puddle-vault-key - -"]);
}

#[tokio::test]
async fn a_machine_with_a_usable_tpm_gets_the_tpm_tag() {
    let setup = Setup::new();
    let (keys, _) = setup.keys(&format!("{HEALTHY}[has-tpm2]\nexit=0\n"));
    let found = keys.find_or_make().await.unwrap();
    assert_eq!(found.source, KeySource::SystemdSealed);
    assert_eq!(found.tag, KeySourceTag::SystemdHostTpm2);
}

#[tokio::test]
async fn the_key_file_is_the_fallback_where_sealing_cannot_work() {
    // Each case is something a real machine said when sealing: systemd 255 (no `--user`: exit 1
    // and the text), the varlink service refusing, an unreadable answer, a seal that does not open
    // again, a tool that hangs.
    let cases: [(&str, String); 6] = [
        (
            "systemd 255",
            "[--version]\nstdout=systemd 255 (255.4-1ubuntu8.18)\\n\n\
             [encrypt]\nexit=1\nstderr=systemd-creds: unrecognized option '--user'\\n\n"
                .to_owned(),
        ),
        (
            "service refused",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n\
             [encrypt]\nexit=1\nstderr=Failed to determine local credential host secret: Permission denied\\n\n"
                .to_owned(),
        ),
        (
            "encrypt prints nothing",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n[encrypt]\n".to_owned(),
        ),
        (
            "the seal does not open again",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n\
             [encrypt]\nstdout=c2VhbGVk\n\
             [decrypt]\nexit=1\nstderr=Failed to decrypt: io.systemd.System\\n\n"
                .to_owned(),
        ),
        (
            "it opens as another key",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n\
             [encrypt]\nstdout=c2VhbGVk\n\
             [decrypt]\nstdout=0123456789abcdef0123456789abcdef\n"
                .to_owned(),
        ),
        (
            "it hangs",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n[encrypt]\nsleep_ms=5000\n".to_owned(),
        ),
    ];
    for (name, behaviour) in cases {
        let setup = Setup::new();
        let (keys, _) = setup.keys(&behaviour);
        let found = keys.find_or_make().await.unwrap();
        assert_eq!(found.source, KeySource::KeyFile, "{name}");
        assert_eq!(found.tag, KeySourceTag::KeyFile, "{name}");
        assert!(!setup.sealed().exists(), "{name}: a sealed file was left");
        assert_eq!(mode_of(&setup.key_file()), 0o600, "{name}");
    }
}

#[tokio::test]
async fn systemd_before_256_is_not_asked_to_seal() {
    let setup = Setup::new();
    let (keys, tool) = setup.keys(
        "[--version]\nstdout=systemd 255 (255.4-1ubuntu8.18)\\n\n\
         [encrypt]\nstderr=should not run\\n\n",
    );
    assert_eq!(
        keys.find_or_make().await.unwrap().source,
        KeySource::KeyFile
    );
    assert_eq!(calls(&Fakes::log(&tool)), ["--version"]);
}

#[tokio::test]
async fn a_sealed_key_that_does_not_open_says_why_and_is_never_replaced() {
    // The first column is what `decrypt` printed on a real machine, the second what Puddle
    // reports.
    let cases: [(&str, &str, Unseal); 6] = [
        (
            "secure boot or machine id changed",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n[decrypt]\nexit=1\nstderr=Failed to decrypt: io.systemd.System\\n\n",
            Unseal::Rejected,
        ),
        (
            "sealed under another name",
            "[--version]\nstdout=systemd 259 (259.5)\\n\n[decrypt]\nexit=1\nstderr=Name in credential doesn't match expectations.\\n\n",
            Unseal::NameMismatch,
        ),
        (
            "the machine went back to systemd 255",
            "[--version]\nstdout=systemd 255 (255.4)\\n\n[decrypt]\nexit=1\nstderr=systemd-creds: unrecognized option '--user'\\n\n",
            Unseal::Unsupported,
        ),
        (
            "the service is not reachable",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n[decrypt]\nexit=1\nstderr=Failed to connect to io.systemd.Credentials\\n\n",
            Unseal::Failed,
        ),
        (
            "it answers something that is not a key",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n[decrypt]\nstdout=short\n",
            Unseal::NotAKey,
        ),
        (
            "it hangs",
            "[--version]\nstdout=systemd 257 (257.13)\\n\n[decrypt]\nsleep_ms=5000\n",
            Unseal::TimedOut,
        ),
    ];
    for (name, behaviour, expected) in cases {
        let setup = Setup::new();
        puddle_fs::private::write_atomic(&setup.sealed(), b"sealed").unwrap();
        let (keys, _) = setup.keys(behaviour);
        assert_eq!(
            keys.find_or_make().await.unwrap_err(),
            KeyError::Sealed(expected),
            "{name}"
        );
        assert!(!setup.key_file().exists(), "{name}: a key file replaced it");
        assert_eq!(std::fs::read(setup.sealed()).unwrap(), b"sealed", "{name}");
    }
}

#[tokio::test]
async fn a_sealed_vault_refuses_a_key_file_and_a_key_file_vault_refuses_a_seal() {
    let setup = Setup::new();
    FileVault::create(
        &setup.vault(),
        puddle_secrets::VaultKey::from_bytes([1; 32]),
        KeySourceTag::SystemdHostTpm2,
    )
    .unwrap();
    puddle_fs::private::write_atomic(&setup.key_file(), &[1; 32]).unwrap();
    let (keys, tool) = setup.keys(HEALTHY);
    assert_eq!(
        keys.find_or_make().await.unwrap_err(),
        KeyError::WrongSource {
            header: KeySourceTag::SystemdHostTpm2,
            found: KeySource::KeyFile
        }
    );

    let setup = Setup::new();
    FileVault::create(
        &setup.vault(),
        puddle_secrets::VaultKey::from_bytes([1; 32]),
        KeySourceTag::KeyFile,
    )
    .unwrap();
    puddle_fs::private::write_atomic(&setup.sealed(), b"sealed").unwrap();
    let (keys, tool_two) = setup.keys(HEALTHY);
    assert_eq!(
        keys.find_or_make().await.unwrap_err(),
        KeyError::WrongSource {
            header: KeySourceTag::KeyFile,
            found: KeySource::SystemdSealed
        }
    );
    // Refused before the tool is asked to open anything.
    assert_eq!(Fakes::log(&tool), "");
    assert_eq!(Fakes::log(&tool_two), "");
}

#[tokio::test]
async fn a_cred_left_by_a_crash_before_the_vault_was_made_is_opened_not_replaced() {
    let setup = Setup::new();
    let (keys, _) = setup.keys(&format!("{HEALTHY}[has-tpm2]\nexit=0\n"));
    let first = keys.find_or_make().await.unwrap();
    let sealed_before = std::fs::read(setup.sealed()).unwrap();
    // No vault was made: the next start finds the same key and picks the tag again.
    let (keys, _) = setup.keys(&format!("{HEALTHY}[has-tpm2]\nexit=0\n"));
    let again = keys.find_or_make().await.unwrap();
    assert_eq!(again.key.expose(), first.key.expose());
    assert_eq!(again.tag, KeySourceTag::SystemdHostTpm2);
    assert_eq!(std::fs::read(setup.sealed()).unwrap(), sealed_before);
}

/// The real tool, where this computer has systemd 256 or later: seal, open, and the refusals for
/// a name that differs. Skipped elsewhere, saying so.
#[tokio::test]
async fn the_real_systemd_creds_seals_and_opens_a_key() {
    let tools = ToolPaths::resolve();
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let keys = VaultKeys::new(dir.path(), &vault, tools).with_credentials_dir(None);
    let found = keys.find_or_make().await.unwrap();
    if found.source != KeySource::SystemdSealed {
        // skip-ok: GitHub's Linux runners ship systemd 255 (no per-user sealing); the host check and a
        // developer machine with systemd 256 or later run this for real.
        eprintln!("skipped: no systemd-creds 256 or later that seals for a user here");
        return;
    }
    assert_ne!(found.tag, KeySourceTag::KeyFile);
    let sealed = dir.path().join("puddle").join("vault-key.cred");
    assert_eq!(mode_of(&sealed), 0o600);
    // The sealed file does not hold the key.
    let bytes = std::fs::read(&sealed).unwrap();
    assert!(!bytes.windows(32).any(|w| w == found.key.expose()));

    let vault_file = FileVault::create(&vault, found.key, found.tag).unwrap();
    let id = StoredId::new("real-seal").unwrap();
    vault_file
        .set(&id, &Secret::new("value".to_owned()))
        .unwrap();
    drop(vault_file);

    let again = VaultKeys::new(dir.path(), &vault, ToolPaths::resolve())
        .with_credentials_dir(None)
        .find_or_make()
        .await
        .unwrap();
    assert_eq!(again.source, KeySource::SystemdSealed);
    let reopened = FileVault::open(&vault, again.key).unwrap();
    assert_eq!(reopened.get(&id).unwrap().unwrap().expose(), "value");

    // A cred that is not ours is refused by the service, with one of its generic answers.
    std::fs::write(&sealed, b"not a credential").unwrap();
    let err = VaultKeys::new(dir.path(), &vault, ToolPaths::resolve())
        .with_credentials_dir(None)
        .find_or_make()
        .await
        .unwrap_err();
    assert!(matches!(err, KeyError::Sealed(_)), "{err:?}");
}
