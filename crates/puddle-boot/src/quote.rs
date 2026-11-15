// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quoting for the plan, which `boot.sh` sources as POSIX sh. Everything Puddle puts into the plan
//! goes through one of these two functions, so no value can end a word or run a command.

use std::fmt::Write as _;

/// `s` as one single-quoted shell word: `it's` becomes `'it'\''s'`.
pub(crate) fn sh_word(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// `bytes` as a single-quoted `printf` format that prints exactly `bytes`. Printable ASCII stays
/// as it is, except the bytes `printf` or the quoting would interpret (`%`, `\`, `'`) and `-` (a
/// leading `-` could read as an option to some `printf`s); every other byte becomes a three-digit
/// octal escape, which POSIX `printf` formats support.
pub(crate) fn printf_format(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 2);
    out.push('\'');
    for &b in bytes {
        if (0x20..0x7f).contains(&b) && !matches!(b, b'%' | b'\\' | b'\'' | b'-') {
            out.push(char::from(b));
        } else {
            // Writing to a String can't fail.
            let _ = write!(out, "\\{b:03o}");
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests;
