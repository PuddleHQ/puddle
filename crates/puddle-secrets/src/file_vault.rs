// SPDX-License-Identifier: GPL-3.0-or-later
//! Puddle's own encrypted file, for a computer whose credential store cannot be used (a headless
//! Linux server, WSL, a container). One file, every value encrypted with AES-256-GCM under a key the
//! caller supplies; where that key comes from is not this module's business.
//!
//! The whole file is rewritten on every change, to a temporary file that is synced and renamed over
//! the old one, so a crash leaves the old vault or the new one, never half of either.
//!
//! # Format, version 1
//!
//! All numbers are big endian.
//!
//! | Part | Bytes |
//! |---|---|
//! | magic | `PDLV` |
//! | format version | 1 |
//! | key source tag | 1 (see [`KeySourceTag`]) |
//! | vault id | 16 random bytes, made once |
//! | key check | 12-byte nonce, then the 48 bytes of AES-256-GCM over a fixed text |
//! | entry count | 4 |
//! | per entry | id length (2), id, nonce (12), ciphertext length (4), ciphertext with its 16-byte tag |
//!
//! The key check's additional data is magic, version, source tag and vault id, so a changed header
//! fails at the key check and a wrong key fails there before any entry is read. An entry's
//! additional data is vault id, version and its id, so an entry copied under another id, or into
//! another vault, fails. The id is in the clear, so a changed id byte makes the value look absent
//! rather than failing; nothing here defends against someone who can write the file deleting
//! entries from it, as they could delete the file.
//!
//! A reader that meets a version byte it does not know refuses ([`VaultError::UnknownVersion`]); a
//! later format gets a new number and the old one stays readable.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use aes_gcm::aead::KeyInit;
use aes_gcm::{AeadInOut, Aes256Gcm, Nonce};
use zeroize::{Zeroize, Zeroizing};

use crate::name::StoredId;
use crate::secret::Secret;
use crate::store::{SecretStore, StoreError};

const MAGIC: &[u8; 4] = b"PDLV";
const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const ID_LEN: usize = 16;
/// Fixed 32 bytes the key check encrypts.
const CHECK_TEXT: &[u8; 32] = b"Puddle vault key check, version1";
const CHECK_LABEL: &[u8] = b"key check";
/// Magic, version, source tag, vault id, key check (nonce and ciphertext with tag), entry count.
const HEADER_LEN: usize = 4 + 1 + 1 + ID_LEN + NONCE_LEN + CHECK_TEXT.len() + TAG_LEN + 4;

/// Where the key that opens a vault comes from; written to the vault's header so a later start can
/// refuse a source weaker than the one that sealed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySourceTag {
    /// Sealed by `systemd-creds` with the host key.
    SystemdHost,
    /// Sealed by `systemd-creds` with the host key and the TPM2 chip.
    SystemdHostTpm2,
    /// A 0600 file in the user's config folder.
    KeyFile,
}

impl KeySourceTag {
    fn byte(self) -> u8 {
        match self {
            Self::SystemdHost => 1,
            Self::SystemdHostTpm2 => 2,
            Self::KeyFile => 3,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::SystemdHost),
            2 => Some(Self::SystemdHostTpm2),
            3 => Some(Self::KeyFile),
            _ => None,
        }
    }

    /// The name the doctor and the problems use.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemdHost => "systemd-host",
            Self::SystemdHostTpm2 => "systemd-host-tpm2",
            Self::KeyFile => "key-file",
        }
    }
}

/// The 32-byte key of a vault. Zeroized on drop; `Debug` prints `<redacted>`; no `Clone`.
pub struct VaultKey(Zeroizing<[u8; 32]>);

impl VaultKey {
    /// Wraps `bytes`. The caller's copy is the caller's to wipe.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// A new random key.
    ///
    /// # Errors
    /// [`VaultError::Random`] when the operating system's random source fails.
    pub fn generate() -> Result<Self, VaultError> {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        getrandom::fill(&mut *bytes).map_err(|_| VaultError::Random)?;
        Ok(Self(bytes))
    }

    /// The key bytes, for the code that seals the key somewhere. Keep the borrow short.
    #[must_use]
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(&aes_gcm::Key::<Aes256Gcm>::from(*self.0))
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Why a vault could not be opened or made. Carries no value, key or path content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// The file system refused (missing folder, permissions, a full disk).
    #[error("the vault file could not be read or written")]
    Io,
    /// A vault already exists at the path.
    #[error("a vault already exists here")]
    Exists,
    /// There is no vault at the path.
    #[error("there is no vault here")]
    Missing,
    /// The file is not a Puddle vault.
    #[error("the file is not a Puddle vault")]
    NotAVault,
    /// The format version is newer than this program knows.
    #[error("the vault has format version {0}, which this program does not know")]
    UnknownVersion(u8),
    /// The key does not open this vault.
    #[error("the key does not open this vault")]
    WrongKey,
    /// The file is cut short or its lengths do not fit.
    #[error("the vault file is damaged")]
    Corrupt,
    /// The operating system's random source failed.
    #[error("the system random source failed")]
    Random,
}

/// An encrypted file of secrets, one per [`StoredId`].
pub struct FileVault {
    path: PathBuf,
    key: VaultKey,
    source: KeySourceTag,
    vault_id: [u8; ID_LEN],
    /// Orders the changes of this process; across processes the data-folder lock does.
    write: Mutex<()>,
}

struct Entry {
    id: String,
    nonce: [u8; NONCE_LEN],
    sealed: Vec<u8>,
}

struct Loaded {
    source: KeySourceTag,
    vault_id: [u8; ID_LEN],
    entries: Vec<Entry>,
}

impl FileVault {
    /// Makes a new, empty vault at `path` (its folder is made owner-only if missing).
    ///
    /// # Errors
    /// [`VaultError::Exists`] when there is a file at `path`, [`VaultError::Io`] or
    /// [`VaultError::Random`] otherwise.
    pub fn create(path: &Path, key: VaultKey, source: KeySourceTag) -> Result<Self, VaultError> {
        if path.exists() {
            return Err(VaultError::Exists);
        }
        let mut vault_id = [0_u8; ID_LEN];
        getrandom::fill(&mut vault_id).map_err(|_| VaultError::Random)?;
        let vault = Self {
            path: path.to_owned(),
            key,
            source,
            vault_id,
            write: Mutex::new(()),
        };
        vault.commit(&[])?;
        Ok(vault)
    }

    /// Opens the vault at `path` and checks `key` against it.
    ///
    /// # Errors
    /// [`VaultError::Missing`], [`VaultError::NotAVault`], [`VaultError::UnknownVersion`],
    /// [`VaultError::WrongKey`] (also when the header was changed), [`VaultError::Corrupt`] or
    /// [`VaultError::Io`].
    pub fn open(path: &Path, key: VaultKey) -> Result<Self, VaultError> {
        let mut vault = Self {
            path: path.to_owned(),
            key,
            source: KeySourceTag::KeyFile,
            vault_id: [0; ID_LEN],
            write: Mutex::new(()),
        };
        let loaded = vault.load()?;
        vault.source = loaded.source;
        vault.vault_id = loaded.vault_id;
        Ok(vault)
    }

    /// Where the key that sealed this vault came from, as its header says.
    #[must_use]
    pub fn source(&self) -> KeySourceTag {
        self.source
    }

    /// The ids held, sorted. Reads no value.
    ///
    /// # Errors
    /// [`StoreError`] when the file cannot be read or is damaged.
    pub fn ids(&self) -> Result<Vec<String>, StoreError> {
        Ok(self
            .load()
            .map_err(|_| StoreError)?
            .entries
            .into_iter()
            .map(|entry| entry.id)
            .collect())
    }

    fn load(&self) -> Result<Loaded, VaultError> {
        let bytes = Zeroizing::new(match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(VaultError::Missing),
            Err(_) => return Err(VaultError::Io),
        });
        self.parse(&bytes)
    }

    fn parse(&self, bytes: &[u8]) -> Result<Loaded, VaultError> {
        let mut at = Reader(bytes);
        if at.take(4)? != MAGIC {
            return Err(VaultError::NotAVault);
        }
        let version = u8::from_be_bytes(at.array()?);
        if version != VERSION {
            return Err(VaultError::UnknownVersion(version));
        }
        let tag = u8::from_be_bytes(at.array()?);
        let vault_id: [u8; ID_LEN] = at.array()?;
        let check_nonce: [u8; NONCE_LEN] = at.array()?;
        let mut check = Zeroizing::new(at.take(CHECK_TEXT.len() + TAG_LEN)?.to_vec());
        let aad = check_aad(tag, &vault_id);
        let opened =
            self.key
                .cipher()
                .decrypt_in_place(&Nonce::from(check_nonce), &aad, &mut *check);
        // A source byte this program does not know is a header that was changed or a newer
        // program's: either way the check cannot vouch for it.
        let source = KeySourceTag::from_byte(tag);
        if opened.is_err() || check.as_slice() != CHECK_TEXT {
            return Err(VaultError::WrongKey);
        }
        let source = source.ok_or(VaultError::Corrupt)?;
        let count = u32::from_be_bytes(at.array()?);
        let mut entries = Vec::new();
        for _ in 0..count {
            let id_len = usize::from(u16::from_be_bytes(at.array()?));
            let id =
                String::from_utf8(at.take(id_len)?.to_vec()).map_err(|_| VaultError::Corrupt)?;
            let nonce: [u8; NONCE_LEN] = at.array()?;
            let sealed_len = usize::try_from(u32::from_be_bytes(at.array()?))
                .map_err(|_| VaultError::Corrupt)?;
            if sealed_len < TAG_LEN {
                return Err(VaultError::Corrupt);
            }
            let sealed = at.take(sealed_len)?.to_vec();
            entries.push(Entry { id, nonce, sealed });
        }
        if !at.0.is_empty() {
            return Err(VaultError::Corrupt);
        }
        Ok(Loaded {
            source,
            vault_id,
            entries,
        })
    }

    /// The whole file for `entries`, sorted by id so the same content is the same bytes.
    fn serialize(&self, entries: &[Entry]) -> Result<Vec<u8>, VaultError> {
        let mut out = Vec::with_capacity(
            HEADER_LEN
                + entries
                    .iter()
                    .map(|e| 18 + e.id.len() + e.sealed.len())
                    .sum::<usize>(),
        );
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(self.source.byte());
        out.extend_from_slice(&self.vault_id);
        let nonce = fresh_nonce()?;
        let mut check = Zeroizing::new(CHECK_TEXT.to_vec());
        self.key
            .cipher()
            .encrypt_in_place(
                &Nonce::from(nonce),
                &check_aad(self.source.byte(), &self.vault_id),
                &mut *check,
            )
            .map_err(|_| VaultError::Corrupt)?;
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&check);
        let count = u32::try_from(entries.len()).map_err(|_| VaultError::Corrupt)?;
        out.extend_from_slice(&count.to_be_bytes());
        for entry in entries {
            let id_len = u16::try_from(entry.id.len()).map_err(|_| VaultError::Corrupt)?;
            let sealed_len = u32::try_from(entry.sealed.len()).map_err(|_| VaultError::Corrupt)?;
            out.extend_from_slice(&id_len.to_be_bytes());
            out.extend_from_slice(entry.id.as_bytes());
            out.extend_from_slice(&entry.nonce);
            out.extend_from_slice(&sealed_len.to_be_bytes());
            out.extend_from_slice(&entry.sealed);
        }
        Ok(out)
    }

    /// Writes the vault for `entries` in place of the old file.
    fn commit(&self, entries: &[Entry]) -> Result<(), VaultError> {
        let mut sorted: Vec<&Entry> = entries.iter().collect();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        let owned: Vec<Entry> = sorted
            .into_iter()
            .map(|e| Entry {
                id: e.id.clone(),
                nonce: e.nonce,
                sealed: e.sealed.clone(),
            })
            .collect();
        let bytes = self.serialize(&owned)?;
        let tmp = self.write_temp(&bytes)?;
        self.rename_over(&tmp)
    }

    /// Step one of a change: the new content in `<vault>.tmp`, synced. The old vault is untouched.
    fn write_temp(&self, bytes: &[u8]) -> Result<PathBuf, VaultError> {
        let dir = self.path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(dir) = dir {
            puddle_fs::private::create_dir(dir).map_err(|_| VaultError::Io)?;
        }
        let mut tmp_name = self.path.as_os_str().to_owned();
        tmp_name.push(".tmp");
        let tmp = PathBuf::from(tmp_name);
        // A leftover from a crash is ours and holds only ciphertext.
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(VaultError::Io),
        }
        let written = (|| {
            let mut file = puddle_fs::private::create_file(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
            return Err(VaultError::Io);
        }
        Ok(tmp)
    }

    /// Step two: the new file takes the old one's place, and the folder is synced.
    fn rename_over(&self, tmp: &Path) -> Result<(), VaultError> {
        let renamed = if self.path.exists() {
            puddle_fs::private::replace_file(tmp, &self.path)
        } else {
            fs::rename(tmp, &self.path)
        };
        if renamed.is_err() {
            let _ = fs::remove_file(tmp);
            return Err(VaultError::Io);
        }
        sync_folder(&self.path);
        Ok(())
    }

    fn aad(&self, id: &str) -> Vec<u8> {
        let mut aad = Vec::with_capacity(ID_LEN + 1 + id.len());
        aad.extend_from_slice(&self.vault_id);
        aad.push(VERSION);
        aad.extend_from_slice(id.as_bytes());
        aad
    }
}

impl SecretStore for FileVault {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        let loaded = self.load().map_err(|_| StoreError)?;
        let Some(entry) = loaded.entries.iter().find(|e| e.id == id.as_str()) else {
            return Ok(None);
        };
        let mut buffer = Zeroizing::new(entry.sealed.clone());
        self.key
            .cipher()
            .decrypt_in_place(
                &Nonce::from(entry.nonce),
                &self.aad(&entry.id),
                &mut *buffer,
            )
            .map_err(|_| StoreError)?;
        match String::from_utf8(std::mem::take(&mut *buffer)) {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(err) => {
                err.into_bytes().zeroize();
                Err(StoreError)
            }
        }
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        let _guard = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let mut loaded = self.load().map_err(|_| StoreError)?;
        let nonce = fresh_nonce().map_err(|_| StoreError)?;
        let mut sealed = secret.expose().as_bytes().to_vec();
        let encrypted = self.key.cipher().encrypt_in_place(
            &Nonce::from(nonce),
            &self.aad(id.as_str()),
            &mut sealed,
        );
        if encrypted.is_err() {
            sealed.zeroize();
            return Err(StoreError);
        }
        let entry = Entry {
            id: id.as_str().to_owned(),
            nonce,
            sealed,
        };
        match loaded.entries.iter_mut().find(|e| e.id == entry.id) {
            Some(slot) => *slot = entry,
            None => loaded.entries.push(entry),
        }
        self.commit(&loaded.entries).map_err(|_| StoreError)
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        let _guard = self.write.lock().unwrap_or_else(PoisonError::into_inner);
        let mut loaded = self.load().map_err(|_| StoreError)?;
        let before = loaded.entries.len();
        loaded.entries.retain(|e| e.id != id.as_str());
        if loaded.entries.len() == before {
            return Ok(());
        }
        self.commit(&loaded.entries).map_err(|_| StoreError)
    }
}

fn check_aad(tag: u8, vault_id: &[u8; ID_LEN]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(MAGIC.len() + 2 + ID_LEN + CHECK_LABEL.len());
    aad.extend_from_slice(MAGIC);
    aad.push(VERSION);
    aad.push(tag);
    aad.extend_from_slice(vault_id);
    aad.extend_from_slice(CHECK_LABEL);
    aad
}

fn fresh_nonce() -> Result<[u8; NONCE_LEN], VaultError> {
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| VaultError::Random)?;
    Ok(nonce)
}

/// Makes the rename itself durable. Windows has no folder to sync.
fn sync_folder(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .and_then(|dir| File::open(dir).ok())
    {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// A cursor over the file's bytes that refuses to run past the end.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], VaultError> {
        if self.0.len() < len {
            return Err(VaultError::Corrupt);
        }
        let (head, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], VaultError> {
        self.take(N)?.try_into().map_err(|_| VaultError::Corrupt)
    }
}

#[cfg(test)]
mod tests;
