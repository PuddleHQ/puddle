// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

/// A header with the fields this reader looks at and nothing else.
fn image(magic: u16, reserve: u64) -> Vec<u8> {
    let pe = 0x80_usize;
    let mut bytes = vec![0_u8; pe + OPTIONAL_HEADER_OFFSET + STACK_RESERVE_OFFSET + 8];
    bytes[PE_OFFSET_FIELD..PE_OFFSET_FIELD + 4].copy_from_slice(&0x80_u32.to_le_bytes());
    bytes[pe..pe + 4].copy_from_slice(b"PE\0\0");
    let optional = pe + OPTIONAL_HEADER_OFFSET;
    bytes[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
    let at = optional + STACK_RESERVE_OFFSET;
    bytes[at..at + 8].copy_from_slice(&reserve.to_le_bytes());
    bytes
}

#[test]
fn the_reserve_of_a_64_bit_program_is_read() {
    assert_eq!(
        pe_stack_reserve_bytes(&image(PE32_PLUS, 8 * 1024 * 1024)),
        Some(8 * 1024 * 1024)
    );
}

#[test]
fn a_32_bit_header_is_not_read() {
    assert_eq!(pe_stack_reserve_bytes(&image(0x10b, 1)), None);
}

#[test]
fn a_file_that_is_not_a_program_is_not_read() {
    assert_eq!(pe_stack_reserve_bytes(b"\x7fELF"), None);
    assert_eq!(pe_stack_reserve_bytes(&[]), None);
    let mut no_signature = image(PE32_PLUS, 1);
    no_signature[0x80] = b'X';
    assert_eq!(pe_stack_reserve_bytes(&no_signature), None);
}

#[test]
fn a_header_that_points_past_the_file_is_not_read() {
    let mut truncated = image(PE32_PLUS, 1);
    truncated.truncate(0x90);
    assert_eq!(pe_stack_reserve_bytes(&truncated), None);
}

#[test]
fn a_file_cut_off_inside_the_optional_header_is_not_read() {
    let mut cut = image(PE32_PLUS, 1);
    cut.truncate(0x80 + OPTIONAL_HEADER_OFFSET + 10);
    assert_eq!(pe_stack_reserve_bytes(&cut), None);
}
