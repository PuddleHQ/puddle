// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the file vault's key comes from, first one found wins:
//!
//! 1. **A credential the unit passes**: `$CREDENTIALS_DIRECTORY/puddle-vault-key`, 32 raw bytes,
//!    set up by an operator with `LoadCredentialEncrypted=`.
//! 2. **A key Puddle sealed itself**: `<config>/puddle/vault-key.cred`, made at the first start with
//!    `systemd-creds --user encrypt` and opened at each start with `decrypt` (systemd 256 or later).
//! 3. **A key file**: `<config>/puddle/vault.key`, 32 random bytes, refused when anyone else can read it.
//!
//! The key goes through `systemd-creds` on standard input and output, never a file in a shared
//! folder. A key is only ever made when no vault exists yet: a vault whose key is missing, or whose
//! header names another source than the one found, is refused with what was found, so a new key is
//! never made over a vault and a weaker source never opens a vault a stronger one sealed.
//!
//! The first start decides the source: nothing found and no vault means sealing (when systemd 256
//! or later answers and the sealed key opens again at once), else the key file. Two processes
//! starting at once would each make a key; the data-folder lock, which the host holds, prevents it.
//! Only Linux has key sources; other platforms answer [`KeyError::NoKeySource`] until one is added.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use zeroize::{Zeroize, Zeroizing};

use crate::error::{SourceError, Tool};
use crate::file_vault::{FileVault, KeySourceTag, VaultError, VaultKey};
use crate::run::{TOOL_TIMEOUT, ToolPaths, run_with_input};

/// The credential name: the unit's ID and the name sealed into the file must be this (systemd 258
/// and later refuse a credential whose sealed name differs).
const CREDENTIAL_NAME: &str = "puddle-vault-key";
const SEALED_FILE: &str = "vault-key.cred";
const KEY_FILE: &str = "vault.key";
/// The first systemd with `--user` for `systemd-creds` (measured on 255: `unrecognized option`).
const FIRST_USER_SCOPE: u32 = 256;
const KEY_LEN: usize = 32;

/// Where a key was found or made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// `$CREDENTIALS_DIRECTORY/puddle-vault-key`, passed by the unit.
    UnitCredential,
    /// `vault-key.cred`, sealed by `systemd-creds --user`.
    SystemdSealed,
    /// `vault.key`, a file only its owner can read.
    KeyFile,
}

impl KeySource {
    /// The name the doctor and the problems use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnitCredential => "unit-credential",
            Self::SystemdSealed => "systemd-sealed",
            Self::KeyFile => "key-file",
        }
    }

    /// Whether a vault whose header names `tag` may be opened with a key from this source. Both
    /// systemd sources hold a systemd-sealed key, so either fits the two systemd tags.
    #[must_use]
    pub fn fits(self, tag: KeySourceTag) -> bool {
        match self {
            Self::UnitCredential | Self::SystemdSealed => tag != KeySourceTag::KeyFile,
            Self::KeyFile => tag == KeySourceTag::KeyFile,
        }
    }
}

/// A key and where it came from.
#[derive(Debug)]
pub struct FoundKey {
    /// The vault key.
    pub key: VaultKey,
    /// The source it came from.
    pub source: KeySource,
    /// The tag a new vault made with this key gets; an existing vault's own tag, when there is one.
    pub tag: KeySourceTag,
}

/// Why a sealed key could not be opened. `systemd-creds` says little, so the causes overlap: a
/// changed Secure Boot setting, a changed machine id and a lost host key all end as
/// [`Unseal::Rejected`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unseal {
    /// `systemd-creds` is not installed.
    #[error("systemd-creds is not installed")]
    ToolMissing,
    /// This systemd has no per-user sealing (before 256).
    #[error("this systemd is older than 256, which has no per-user sealing")]
    Unsupported,
    /// The service refused: the host key is gone or changed (a cloned image, a restore onto another
    /// machine, a new machine id) or the TPM2 state no longer matches (a Secure Boot change).
    #[error(
        "the sealed key does not open on this machine (a changed machine id, a lost host key or a changed Secure Boot setting)"
    )]
    Rejected,
    /// The name sealed into the file is not the one Puddle asks for.
    #[error("the sealed key was made under another name")]
    NameMismatch,
    /// `systemd-creds` ran and failed in another way (its service is not reachable).
    #[error("systemd-creds failed, and its service may not be reachable")]
    Failed,
    /// `systemd-creds` did not answer in time.
    #[error("systemd-creds did not answer in time")]
    TimedOut,
    /// It answered, but not with a 32-byte key.
    #[error("systemd-creds answered something that is not a vault key")]
    NotAKey,
}

/// Why no vault key could be had. Carries no key and no path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// This platform has no key source yet.
    #[error("this platform has no vault key source")]
    NoKeySource,
    /// The key found is of another kind than the one that sealed the vault.
    #[error(
        "the vault was sealed with a {} key but a {} key was found",
        .header.as_str(),
        .found.as_str()
    )]
    WrongSource {
        /// What the vault's header names.
        header: KeySourceTag,
        /// What was found.
        found: KeySource,
    },
    /// A vault exists and no key source has its key.
    #[error("the vault was sealed with a {} key and that key is not here", .0.as_str())]
    KeyMissing(KeySourceTag),
    /// The unit's credential is not 32 bytes.
    #[error("the credential the unit passes is not a 32-byte key")]
    UnitCredentialInvalid,
    /// The key file is not owner-only, or its permissions cannot be read.
    #[error("the vault key file is not safe to use: {0}; fix its permissions")]
    KeyFileUnsafe(String),
    /// The key file is not 32 bytes.
    #[error("the vault key file is not a 32-byte key")]
    KeyFileInvalid,
    /// The sealed key exists and does not open.
    #[error("the sealed vault key cannot be opened: {0}")]
    Sealed(Unseal),
    /// The vault's header could not be read.
    #[error("the vault's header could not be read: {0}")]
    Vault(VaultError),
    /// A key file could not be read or written.
    #[error("a vault key file could not be read or written")]
    Io,
    /// The operating system's random source failed.
    #[error("the system random source failed")]
    Random,
}

/// The places a vault key is looked for, and made in.
#[derive(Debug, Clone)]
pub struct VaultKeys {
    config_dir: PathBuf,
    vault: PathBuf,
    credentials_dir: Option<PathBuf>,
    tools: ToolPaths,
    timeout: Duration,
}

impl VaultKeys {
    /// Keys for the vault at `vault`, kept under `<config_home>/puddle` (the user's config folder,
    /// deliberately not the data folder, so a copy of the data folder holds only ciphertext).
    /// `$CREDENTIALS_DIRECTORY` is read now.
    #[must_use]
    pub fn new(config_home: &Path, vault: &Path, tools: ToolPaths) -> Self {
        Self {
            config_dir: config_home.join("puddle"),
            vault: vault.to_path_buf(),
            credentials_dir: std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from),
            tools,
            timeout: TOOL_TIMEOUT,
        }
    }

    /// The same with the unit's credential folder set (tests); `None` means no credential.
    #[must_use]
    pub fn with_credentials_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.credentials_dir = dir;
        self
    }

    /// The same with another limit for each `systemd-creds` call (tests).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Finds the key, or makes one when there is no vault and none of the sources has a key.
    ///
    /// # Errors
    /// [`KeyError`]: a different source than the vault's, a missing or unreadable key, or a key
    /// file others can read.
    pub async fn find_or_make(&self) -> Result<FoundKey, KeyError> {
        cfg!(target_os = "linux")
            .then_some(())
            .ok_or(KeyError::NoKeySource)?;
        let header = FileVault::header_source(&self.vault).map_err(KeyError::Vault)?;
        if let Some(present) = self.first_present() {
            if let Some(tag) = header
                && !present.fits(tag)
            {
                return Err(KeyError::WrongSource {
                    header: tag,
                    found: present,
                });
            }
            return self.read(present, header).await;
        }
        if let Some(tag) = header {
            return Err(KeyError::KeyMissing(tag));
        }
        self.make().await
    }

    fn unit_credential(&self) -> Option<PathBuf> {
        let dir = self
            .credentials_dir
            .as_deref()
            .filter(|d| d.is_absolute())?;
        Some(dir.join(CREDENTIAL_NAME))
    }

    fn sealed_path(&self) -> PathBuf {
        self.config_dir.join(SEALED_FILE)
    }

    fn key_file_path(&self) -> PathBuf {
        self.config_dir.join(KEY_FILE)
    }

    /// The first source, in order, that has something; nothing is read.
    fn first_present(&self) -> Option<KeySource> {
        if self.unit_credential().is_some_and(|p| p.is_file()) {
            Some(KeySource::UnitCredential)
        } else if self.sealed_path().is_file() {
            Some(KeySource::SystemdSealed)
        } else if self.key_file_path().is_file() {
            Some(KeySource::KeyFile)
        } else {
            None
        }
    }

    async fn read(
        &self,
        source: KeySource,
        header: Option<KeySourceTag>,
    ) -> Result<FoundKey, KeyError> {
        let (key, tag) = match source {
            KeySource::UnitCredential => {
                let path = self.unit_credential().ok_or(KeyError::Io)?;
                let bytes = Zeroizing::new(fs::read(path).map_err(|_| KeyError::Io)?);
                let key = key_from(&bytes).ok_or(KeyError::UnitCredentialInvalid)?;
                (key, header.unwrap_or(KeySourceTag::SystemdHost))
            }
            KeySource::SystemdSealed => {
                let sealed = fs::read(self.sealed_path()).map_err(|_| KeyError::Io)?;
                let program = self.program().map_err(KeyError::Sealed)?;
                let key = self
                    .unseal(program, &sealed)
                    .await
                    .map_err(KeyError::Sealed)?;
                let tag = match header {
                    Some(tag) => tag,
                    None => self.seal_tag(program).await,
                };
                (key, tag)
            }
            KeySource::KeyFile => (read_key_file(&self.key_file_path())?, KeySourceTag::KeyFile),
        };
        Ok(FoundKey { key, source, tag })
    }

    async fn make(&self) -> Result<FoundKey, KeyError> {
        let key = VaultKey::generate().map_err(|_| KeyError::Random)?;
        if let Some(tag) = self.try_seal(&key).await {
            return Ok(FoundKey {
                key,
                source: KeySource::SystemdSealed,
                tag,
            });
        }
        // A key file is made only where sealing is not possible, so the weaker source is the
        // fallback and never the choice.
        puddle_fs::private::write_atomic(&self.key_file_path(), key.expose())
            .map_err(|_| KeyError::Io)?;
        Ok(FoundKey {
            key,
            source: KeySource::KeyFile,
            tag: KeySourceTag::KeyFile,
        })
    }

    /// Seals `key` and stores the result when this machine can, and the sealed key opens again at
    /// once (a seal that cannot be opened would lose every value in the vault). Any failure answers
    /// `None` and leaves nothing behind.
    async fn try_seal(&self, key: &VaultKey) -> Option<KeySourceTag> {
        let program = self.tools.get(Tool::SystemdCreds).ok()?;
        if let Ok(output) = run_with_input(
            Tool::SystemdCreds,
            program,
            &["--version"],
            None,
            self.timeout,
        )
        .await
            && output.success
            && major_version(&output.stdout).is_some_and(|v| v < FIRST_USER_SCOPE)
        {
            return None;
        }
        let output = run_with_input(
            Tool::SystemdCreds,
            program,
            &[
                "--user",
                "encrypt",
                &format!("--name={CREDENTIAL_NAME}"),
                "-",
                "-",
            ],
            Some(key.expose()),
            self.timeout,
        )
        .await
        .ok()?;
        if !output.success || output.stdout.is_empty() {
            return None;
        }
        let opened = self.unseal(program, &output.stdout).await.ok()?;
        if opened.expose() != key.expose() {
            return None;
        }
        puddle_fs::private::write_atomic(&self.sealed_path(), &output.stdout).ok()?;
        Some(self.seal_tag(program).await)
    }

    /// Whether the service seals with the TPM2 too: it does when a TPM2 is fully usable and the
    /// machine is not a container, which `has-tpm2` answers with exit 0.
    async fn seal_tag(&self, program: &Path) -> KeySourceTag {
        match run_with_input(
            Tool::SystemdCreds,
            program,
            &["has-tpm2"],
            None,
            self.timeout,
        )
        .await
        {
            Ok(output) if output.success => KeySourceTag::SystemdHostTpm2,
            _ => KeySourceTag::SystemdHost,
        }
    }

    fn program(&self) -> Result<&Path, Unseal> {
        self.tools
            .get(Tool::SystemdCreds)
            .map_err(|_| Unseal::ToolMissing)
    }

    async fn unseal(&self, program: &Path, sealed: &[u8]) -> Result<VaultKey, Unseal> {
        let output = run_with_input(
            Tool::SystemdCreds,
            program,
            &[
                "--user",
                "decrypt",
                &format!("--name={CREDENTIAL_NAME}"),
                "-",
                "-",
            ],
            Some(sealed),
            self.timeout,
        )
        .await
        .map_err(|err| match err {
            SourceError::ToolMissing(_) => Unseal::ToolMissing,
            SourceError::Timeout(_) => Unseal::TimedOut,
            _ => Unseal::Failed,
        })?;
        if output.success {
            return key_from(&output.stdout).ok_or(Unseal::NotAKey);
        }
        // The version is read from `--version`, not from the English of the refusal.
        let old = run_with_input(
            Tool::SystemdCreds,
            program,
            &["--version"],
            None,
            self.timeout,
        )
        .await
        .is_ok_and(|v| v.success && major_version(&v.stdout).is_some_and(|n| n < FIRST_USER_SCOPE));
        Err(if old {
            Unseal::Unsupported
        } else {
            classify(&output.stderr)
        })
    }
}

/// What a failed `decrypt` says, by the two texts measured on 257 to 259 (systemd gives no other
/// handle); any other text is "failed".
fn classify(stderr: &[u8]) -> Unseal {
    let text = String::from_utf8_lossy(stderr);
    if text.contains("doesn't match expectations") {
        Unseal::NameMismatch
    } else if text.contains("Failed to decrypt") {
        Unseal::Rejected
    } else {
        Unseal::Failed
    }
}

/// The major version in the first line of `systemd-creds --version`: `systemd 257 (257.13-1)`.
fn major_version(stdout: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(stdout).ok()?;
    text.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

/// A key from exactly 32 bytes; the caller's buffer is the caller's to wipe.
fn key_from(bytes: &[u8]) -> Option<VaultKey> {
    let mut array: [u8; KEY_LEN] = bytes.try_into().ok()?;
    let key = VaultKey::from_bytes(array);
    array.zeroize();
    Some(key)
}

/// Reads the key file, refusing one that others can read (as `ssh` does with a private key).
fn read_key_file(path: &Path) -> Result<VaultKey, KeyError> {
    let file = fs::File::open(path).map_err(|_| KeyError::Io)?;
    puddle_fs::private::check(&file).map_err(|err| KeyError::KeyFileUnsafe(err.to_string()))?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_LEN + 1));
    // One byte past a key: a longer file is refused without reading all of it.
    file.take(KEY_LEN as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| KeyError::Io)?;
    key_from(&bytes).ok_or(KeyError::KeyFileInvalid)
}

/// The user's config folder: `$XDG_CONFIG_HOME` when it is an absolute path, else `~/.config`.
#[must_use]
pub fn config_home() -> Option<PathBuf> {
    config_home_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn config_home_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let xdg = xdg.map(PathBuf::from).filter(|p| p.is_absolute());
    xdg.or_else(|| {
        home.map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|h| h.join(".config"))
    })
}

#[cfg(test)]
mod tests;
