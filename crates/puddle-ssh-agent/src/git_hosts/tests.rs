// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::testing::TestKey;
use ssh_key::PublicKey;

fn key(openssh: &str) -> KeyData {
    PublicKey::from_openssh(openssh).unwrap().key_data().clone()
}

#[test]
fn every_git_host_key_is_on_the_list() {
    // The keys as GitHub's `api.github.com/meta` and GitLab's documentation publish them.
    for openssh in [
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=",
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAfuCHKVTjquxvt6CM6tdG4SLp1Btn/nOeHHE5UOzRdf",
    ] {
        assert!(is_git_host_key(&key(openssh)), "{openssh}");
    }
}

#[test]
fn any_other_key_is_not() {
    assert!(!is_git_host_key(&TestKey::ed25519(1).key_data()));
    assert!(!is_git_host_key(&TestKey::ed25519(2).key_data()));
}

#[test]
fn the_list_holds_sha256_fingerprints_only() {
    assert_eq!(PUBLISHED.len(), 10);
    for fingerprint in PUBLISHED {
        let b64 = fingerprint.strip_prefix("SHA256:").unwrap();
        assert_eq!(b64.len(), 43, "{fingerprint}");
    }
}
