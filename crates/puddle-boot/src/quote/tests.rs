// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn words_are_single_quoted_with_quotes_escaped() {
    assert_eq!(sh_word(""), "''");
    assert_eq!(sh_word("a b"), "'a b'");
    assert_eq!(sh_word("it's"), r"'it'\''s'");
    assert_eq!(sh_word("$(x) `y` \\ \n"), "'$(x) `y` \\ \n'");
}

#[test]
fn printf_formats_escape_everything_special() {
    assert_eq!(printf_format(b""), "''");
    assert_eq!(printf_format(b"abc XYZ/09.:="), "'abc XYZ/09.:='");
    assert_eq!(printf_format(b"%s"), r"'\045s'");
    assert_eq!(printf_format(b"\\n"), r"'\134n'");
    assert_eq!(printf_format(b"'"), r"'\047'");
    assert_eq!(printf_format(b"-----BEGIN"), r"'\055\055\055\055\055BEGIN'");
    assert_eq!(printf_format(b"a\nb\0\xff"), r"'a\012b\000\377'");
}

#[test]
fn printf_formats_hold_no_quote_or_control_byte() {
    let all: Vec<u8> = (0..=255).collect();
    let f = printf_format(&all);
    let inner = &f[1..f.len() - 1];
    assert!(!inner.contains('\''));
    assert!(inner.bytes().all(|b| (0x20..0x7f).contains(&b)));
}
