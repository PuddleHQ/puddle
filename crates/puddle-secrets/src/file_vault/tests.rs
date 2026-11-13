// SPDX-License-Identifier: GPL-3.0-or-later
//! The format and the crash safety of the vault. The behaviour every store shares is in
//! `tests/store_contract.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]

use super::*;

const CANARY: &str = "PUDDLE-CANARY-3f9a61c2d4e8b7a0";

fn key(byte: u8) -> VaultKey {
    VaultKey::from_bytes([byte; 32])
}

fn id(name: &str) -> StoredId {
    StoredId::new(name).unwrap()
}

fn put(vault: &FileVault, name: &str, value: &str) {
    vault
        .set(&id(name), &Secret::new(value.to_owned()))
        .unwrap();
}

fn read(vault: &FileVault, name: &str) -> Result<Option<String>, StoreError> {
    vault
        .get(&id(name))
        .map(|found| found.map(|s| s.expose().to_owned()))
}

/// A vault with `a`, `b` and `c` under key 1.
fn three() -> (tempfile::TempDir, PathBuf, FileVault) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets").join("vault");
    let vault = FileVault::create(&path, key(1), KeySourceTag::KeyFile).unwrap();
    put(&vault, "a", "value-a");
    put(&vault, "b", "value-b");
    put(&vault, "c", "value-c");
    (dir, path, vault)
}

/// Byte offset of the first entry's id length field.
const FIRST_ENTRY: usize = HEADER_LEN;

#[test]
fn a_vault_made_and_reopened_keeps_its_values_and_its_key_source() {
    let (_dir, path, vault) = three();
    assert_eq!(vault.source(), KeySourceTag::KeyFile);
    drop(vault);
    let again = FileVault::open(&path, key(1)).unwrap();
    assert_eq!(again.source(), KeySourceTag::KeyFile);
    assert_eq!(read(&again, "b").unwrap().as_deref(), Some("value-b"));
    assert_eq!(again.ids().unwrap(), ["a", "b", "c"]);
}

#[test]
fn creating_over_an_existing_vault_is_refused_and_leaves_it_alone() {
    let (_dir, path, _vault) = three();
    assert_eq!(
        FileVault::create(&path, key(2), KeySourceTag::SystemdHost).err(),
        Some(VaultError::Exists)
    );
    let still = FileVault::open(&path, key(1)).unwrap();
    assert_eq!(read(&still, "a").unwrap().as_deref(), Some("value-a"));
}

#[test]
fn opening_a_missing_file_says_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        FileVault::open(&dir.path().join("vault"), key(1)).err(),
        Some(VaultError::Missing)
    );
}

#[test]
fn the_file_never_holds_a_value_or_the_key_in_the_clear() {
    let (_dir, path, vault) = three();
    put(&vault, "token", CANARY);
    let bytes = fs::read(&path).unwrap();
    let find = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
    assert!(!find(CANARY.as_bytes()));
    assert!(!find(&[1_u8; 32]));
    assert!(!find(b"value-a"));
    assert!(find(b"token"), "ids are listed in the clear on purpose");
    assert_eq!(&bytes[..4], b"PDLV");
    assert_eq!(bytes[4], 1);
}

#[test]
fn a_replaced_or_deleted_value_leaves_no_trace_of_the_old_one() {
    let (_dir, path, vault) = three();
    put(&vault, "token", CANARY);
    put(&vault, "token", "second");
    vault.delete(&id("c")).unwrap();
    let bytes = fs::read(&path).unwrap();
    assert!(!bytes.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()));
    assert_eq!(read(&vault, "token").unwrap().as_deref(), Some("second"));
    assert_eq!(read(&vault, "c").unwrap(), None);
    assert_eq!(vault.ids().unwrap(), ["a", "b", "token"]);
}

#[test]
fn the_same_value_twice_is_two_different_ciphertexts() {
    let (_dir, path, vault) = three();
    put(&vault, "x", "same");
    let first = fs::read(&path).unwrap();
    put(&vault, "x", "same");
    assert_ne!(first, fs::read(&path).unwrap());
}

#[test]
fn a_wrong_key_fails_at_the_key_check() {
    let (_dir, path, _vault) = three();
    assert_eq!(
        FileVault::open(&path, key(2)).err(),
        Some(VaultError::WrongKey)
    );
}

#[test]
fn an_unknown_version_byte_refuses_without_guessing() {
    let (_dir, path, _vault) = three();
    let mut bytes = fs::read(&path).unwrap();
    bytes[4] = 2;
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        FileVault::open(&path, key(1)).err(),
        Some(VaultError::UnknownVersion(2))
    );
}

#[test]
fn a_file_that_is_not_a_vault_is_said_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault");
    fs::write(&path, b"[core]\nbare = false\n and some more text").unwrap();
    assert_eq!(
        FileVault::open(&path, key(1)).err(),
        Some(VaultError::NotAVault)
    );
    fs::write(&path, b"PD").unwrap();
    assert_eq!(
        FileVault::open(&path, key(1)).err(),
        Some(VaultError::Corrupt)
    );
}

#[test]
fn a_flipped_byte_in_the_header_fails_the_open() {
    let (_dir, path, _vault) = three();
    let good = fs::read(&path).unwrap();
    // The source tag, the vault id and every byte of the key check.
    for at in 5..FIRST_ENTRY - 4 {
        let mut bytes = good.clone();
        bytes[at] ^= 0x01;
        fs::write(&path, &bytes).unwrap();
        assert!(
            FileVault::open(&path, key(1)).is_err(),
            "byte {at} changed and the vault still opened"
        );
    }
}

#[test]
fn a_flipped_byte_in_one_entry_fails_that_read_and_no_other() {
    let (_dir, path, vault) = three();
    let good = fs::read(&path).unwrap();
    let loaded = vault.load().unwrap();
    let entry_len = |e: &Entry| 2 + e.id.len() + NONCE_LEN + 4 + e.sealed.len();
    let mut start = FIRST_ENTRY;
    for (index, entry) in loaded.entries.iter().enumerate() {
        // The nonce, the length of the ciphertext excluded, and every ciphertext and tag byte.
        let from = start + 2 + entry.id.len();
        let to = start + entry_len(entry);
        for at in (from..from + NONCE_LEN).chain(from + NONCE_LEN + 4..to) {
            let mut bytes = good.clone();
            bytes[at] ^= 0x80;
            fs::write(&path, &bytes).unwrap();
            let opened = FileVault::open(&path, key(1)).unwrap();
            for (other, name) in ["a", "b", "c"].into_iter().enumerate() {
                let result = read(&opened, name);
                if other == index {
                    assert_eq!(result, Err(StoreError), "entry {name}, byte {at}");
                } else {
                    assert_eq!(result.unwrap().unwrap(), format!("value-{name}"));
                }
            }
        }
        start = to;
    }
}

#[test]
fn an_entry_moved_under_another_id_fails_to_decrypt() {
    let (_dir, path, vault) = three();
    let mut loaded = vault.load().unwrap();
    // Entry a's nonce and ciphertext under the id of b.
    let a = loaded.entries.remove(0);
    loaded.entries[0].nonce = a.nonce;
    loaded.entries[0].sealed = a.sealed;
    fs::write(&path, vault.serialize(&loaded.entries).unwrap()).unwrap();
    let opened = FileVault::open(&path, key(1)).unwrap();
    assert_eq!(read(&opened, "b"), Err(StoreError));
    assert_eq!(read(&opened, "c").unwrap().as_deref(), Some("value-c"));
}

#[test]
fn an_entry_copied_from_another_vault_fails_to_decrypt() {
    let (_dir, _path, vault) = three();
    let other_dir = tempfile::tempdir().unwrap();
    let other_path = other_dir.path().join("vault");
    // The same key, so only the vault id tells them apart.
    let other = FileVault::create(&other_path, key(1), KeySourceTag::KeyFile).unwrap();
    put(&other, "a", "other-value");
    let mut stolen = vault.load().unwrap().entries;
    stolen.truncate(1);
    // Spliced under the other vault's own header, so its key check passes.
    let spliced = other.serialize(&stolen).unwrap();
    fs::write(&other_path, spliced).unwrap();
    let opened = FileVault::open(&other_path, key(1)).unwrap();
    assert_eq!(read(&opened, "a"), Err(StoreError));
}

#[test]
fn a_cut_short_or_padded_file_is_damaged_not_empty() {
    let (_dir, path, _vault) = three();
    let good = fs::read(&path).unwrap();
    for cut in [good.len() - 1, FIRST_ENTRY + 1, FIRST_ENTRY] {
        fs::write(&path, &good[..cut]).unwrap();
        assert_eq!(
            FileVault::open(&path, key(1)).err(),
            Some(VaultError::Corrupt),
            "cut at {cut}"
        );
    }
    let mut padded = good;
    padded.push(0);
    fs::write(&path, padded).unwrap();
    assert_eq!(
        FileVault::open(&path, key(1)).err(),
        Some(VaultError::Corrupt)
    );
}

#[test]
fn a_huge_declared_count_or_length_does_not_allocate_or_panic() {
    let (_dir, path, _vault) = three();
    let mut bytes = fs::read(&path).unwrap();
    bytes[FIRST_ENTRY - 4..FIRST_ENTRY].copy_from_slice(&u32::MAX.to_be_bytes());
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        FileVault::open(&path, key(1)).err(),
        Some(VaultError::Corrupt)
    );
}

#[test]
fn a_crash_between_the_temp_write_and_the_rename_leaves_the_old_vault_readable() {
    let (_dir, path, vault) = three();
    // The first half of `set`, then the process "dies".
    let mut loaded = vault.load().unwrap();
    loaded.entries.retain(|e| e.id != "a");
    let bytes = vault.serialize(&loaded.entries).unwrap();
    let tmp = vault.write_temp(&bytes).unwrap();
    assert!(tmp.exists());

    let after = FileVault::open(&path, key(1)).unwrap();
    assert_eq!(read(&after, "a").unwrap().as_deref(), Some("value-a"));
    assert_eq!(after.ids().unwrap(), ["a", "b", "c"]);

    // The next change replaces the leftover and succeeds.
    put(&after, "d", "value-d");
    assert!(!tmp.exists());
    assert_eq!(after.ids().unwrap(), ["a", "b", "c", "d"]);
}

#[test]
fn a_failed_write_leaves_the_vault_and_no_temp_file() {
    let (_dir, path, vault) = three();
    // A folder where the temp file goes makes the write fail.
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    fs::create_dir(&tmp).unwrap();
    assert_eq!(
        vault.set(&id("z"), &Secret::new("v".to_owned())),
        Err(StoreError)
    );
    fs::remove_dir(&tmp).unwrap();
    assert_eq!(vault.ids().unwrap(), ["a", "b", "c"]);
}

#[cfg(unix)]
#[test]
fn the_vault_and_its_folder_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_dir, path, _vault) = three();
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[test]
fn neither_the_key_nor_an_error_prints_a_value() {
    assert_eq!(format!("{:?}", key(7)), "<redacted>");
    for error in [
        VaultError::Io,
        VaultError::WrongKey,
        VaultError::UnknownVersion(9),
        VaultError::Corrupt,
    ] {
        let text = format!("{error} {error:?}");
        assert!(!text.contains("PUDDLE"));
    }
}

#[test]
fn a_generated_key_is_random_and_each_source_tag_has_a_name() {
    assert_ne!(
        VaultKey::generate().unwrap().expose(),
        VaultKey::generate().unwrap().expose()
    );
    for tag in [
        KeySourceTag::SystemdHost,
        KeySourceTag::SystemdHostTpm2,
        KeySourceTag::KeyFile,
    ] {
        assert_eq!(KeySourceTag::from_byte(tag.byte()), Some(tag));
        assert_ne!(tag.as_str(), "");
    }
    assert_eq!(KeySourceTag::from_byte(0), None);
}

#[test]
fn a_folder_where_the_vault_should_be_is_an_io_error_not_a_missing_vault() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        FileVault::open(dir.path(), key(1)).err(),
        Some(VaultError::Io)
    );
}

#[test]
fn a_vault_under_a_file_instead_of_a_folder_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-folder");
    fs::write(&blocker, b"x").unwrap();
    assert_eq!(
        FileVault::create(&blocker.join("vault"), key(1), KeySourceTag::KeyFile).err(),
        Some(VaultError::Io)
    );
    assert_eq!(fs::read(&blocker).unwrap(), b"x");
}

#[test]
fn an_entry_too_short_to_hold_its_tag_makes_the_file_corrupt() {
    let (_dir, path, vault) = three();
    let short = Entry {
        id: "a".to_owned(),
        nonce: [0; NONCE_LEN],
        sealed: vec![0; TAG_LEN - 1],
    };
    fs::write(&path, vault.serialize(&[short]).unwrap()).unwrap();
    assert_eq!(
        FileVault::open(&path, key(1)).err(),
        Some(VaultError::Corrupt)
    );
}

#[test]
fn an_entry_that_decrypts_to_text_that_is_not_utf8_fails_only_its_own_read() {
    let (_dir, _path, vault) = three();
    let nonce = fresh_nonce().unwrap();
    let mut sealed = vec![0xff, 0xfe, 0xfd];
    vault
        .key
        .cipher()
        .encrypt_in_place(&Nonce::from(nonce), &vault.aad("bad"), &mut sealed)
        .unwrap();
    let mut entries = vault.load().unwrap().entries;
    entries.push(Entry {
        id: "bad".to_owned(),
        nonce,
        sealed,
    });
    vault.commit(&entries).unwrap();
    assert_eq!(read(&vault, "bad"), Err(StoreError));
    assert_eq!(read(&vault, "a").unwrap().as_deref(), Some("value-a"));
}

#[test]
fn a_rename_that_fails_removes_the_temp_file_and_keeps_what_is_there() {
    let (_dir, path, vault) = three();
    let tmp = vault.write_temp(b"new content").unwrap();
    // A folder with something in it cannot be replaced by a file.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("kept"), b"kept").unwrap();
    assert_eq!(vault.rename_over(&tmp), Err(VaultError::Io));
    assert!(!tmp.exists());
    assert_eq!(fs::read(path.join("kept")).unwrap(), b"kept");
}

#[test]
fn an_empty_path_is_an_io_error() {
    assert_eq!(
        FileVault::create(Path::new(""), key(1), KeySourceTag::KeyFile).err(),
        Some(VaultError::Io)
    );
    assert_eq!(
        FileVault::open(Path::new(""), key(1)).err(),
        Some(VaultError::Io)
    );
}
