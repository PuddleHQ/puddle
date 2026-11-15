// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn current_user_sid_is_a_string_sid() {
    let sid = current_user_sid().unwrap();
    assert!(sid.starts_with("S-1-"), "{sid}");
}

#[test]
fn a_malformed_sddl_is_an_error_not_a_null_descriptor() {
    assert!(LocalBox::security_descriptor("D:P(not sddl").is_err());
}

#[test]
fn sddl_grants_only_the_given_sid_and_blocks_inheritance() {
    assert_eq!(
        owner_only_sddl("S-1-5-21-1-2-3-1001", "GA", false),
        "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)"
    );
    assert_eq!(
        owner_only_sddl("S-1-5-21-1-2-3-1001", "FA", true),
        "D:P(A;OICI;FA;;;S-1-5-21-1-2-3-1001)"
    );
}

/// An `ACCESS_ALLOWED_ACE` in a `u32` buffer of `bytes` bytes: a SID of `sub_authorities`
/// sub-authorities from byte 8 on.
fn ace_bytes_of(sub_authorities: u8, bytes: usize) -> Vec<u32> {
    let mut buffer = vec![0u32; bytes.div_ceil(4)];
    let raw = as_bytes_mut(&mut buffer);
    raw[8] = 1; // SID revision
    raw[9] = sub_authorities;
    buffer
}

fn as_bytes(buffer: &[u32]) -> &[u8] {
    // SAFETY: a `u32` slice is valid as bytes of four times the length.
    unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), buffer.len() * 4) }
}

fn as_bytes_mut(buffer: &mut [u32]) -> &mut [u8] {
    // SAFETY: a `u32` slice is valid as bytes of four times the length, and any bytes are a u32.
    unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast(), buffer.len() * 4) }
}

#[test]
fn an_ace_holds_a_sid_only_when_the_whole_sid_is_inside_its_size() {
    let header_and_mask = offset_of!(ACCESS_ALLOWED_ACE, SidStart);
    for sub_authorities in [0u8, 1, 5] {
        let sid = 8 + 4 * usize::from(sub_authorities);
        let size = header_and_mask + sid;
        let whole = ace_bytes_of(sub_authorities, size);
        assert!(allowed_sid(&as_bytes(&whole)[..size]).is_ok());
    }
}

#[test]
fn an_ace_cut_off_inside_its_sid_is_refused() {
    let whole_struct = size_of::<ACCESS_ALLOWED_ACE>();
    // Exactly the struct, 4 bytes of a SID that claims 5 sub-authorities.
    let cut = ace_bytes_of(5, 64);
    assert_eq!(
        allowed_sid(&as_bytes(&cut)[..whole_struct]),
        Err(AceRefusal::TooShort)
    );
    // One byte short of a one-sub-authority SID.
    let short = ace_bytes_of(1, 64);
    assert_eq!(
        allowed_sid(&as_bytes(&short)[..whole_struct + 7]),
        Err(AceRefusal::TooShort)
    );
    // An entry smaller than its own struct, and an empty one.
    let tiny = ace_bytes_of(0, 64);
    assert_eq!(
        allowed_sid(&as_bytes(&tiny)[..whole_struct - 1]),
        Err(AceRefusal::TooShort)
    );
    assert_eq!(allowed_sid(&[]), Err(AceRefusal::TooShort));
}

#[test]
fn an_entry_that_is_not_an_allow_entry_is_refused_by_type() {
    let mut deny = ace_bytes_of(0, 64);
    as_bytes_mut(&mut deny)[0] = 1; // ACCESS_DENIED_ACE_TYPE
    assert_eq!(allowed_sid(as_bytes(&deny)), Err(AceRefusal::NotAllowEntry));
}

#[test]
fn an_access_list_with_a_deny_entry_is_not_owner_only() {
    // Every entry names the current user, so only the entry's type makes it fail.
    let sid = current_user_sid().unwrap();
    let descriptor =
        LocalBox::security_descriptor(&format!("D:P(D;;WD;;;{sid})(A;;FA;;;{sid})")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    drop(create_new_with(&path, &descriptor).unwrap());
    assert_eq!(
        dacl_is_owner_only(&File::open(&path).unwrap()).unwrap(),
        Err("entry 0 is not a plain allow entry".to_owned())
    );
}
