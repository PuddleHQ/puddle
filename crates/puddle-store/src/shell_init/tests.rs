// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn line_ends_become_lf_and_the_text_ends_in_one_newline() {
    assert_eq!(
        ShellInitText::new("a\r\nb\rc").unwrap().as_str(),
        "a\nb\nc\n"
    );
    assert_eq!(ShellInitText::new("a\n").unwrap().as_str(), "a\n");
}

#[test]
fn an_empty_text_a_nul_and_a_long_text_are_refused_and_say_why() {
    let why = |text: &str| ShellInitText::new(text).unwrap_err().to_string();
    assert!(why(" \n\t").contains("empty"));
    assert!(why("a\0b").contains("NUL"));
    assert!(why(&"x".repeat(MAX_SHELL_INIT_BYTES)).contains("at most 48 KiB"));
    assert!(ShellInitText::new(&"x".repeat(MAX_SHELL_INIT_BYTES - 1)).is_ok());
}
