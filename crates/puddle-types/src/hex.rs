// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lower-case hexadecimal of bytes, once for every crate that names a digest, a token or a random id.

use std::fmt::Write as _;

/// Two lower-case hexadecimal digits per byte of `bytes`.
///
/// ```
/// assert_eq!(puddle_types::hex_lower(&[0x00, 0x9f, 0xff]), "009fff");
/// ```
#[must_use]
pub fn hex_lower(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
            // writing into a `String` cannot fail
            let _ = write!(out, "{b:02x}");
            out
        })
}

#[cfg(test)]
mod tests;
