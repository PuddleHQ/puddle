// SPDX-License-Identifier: GPL-3.0-or-later
//! The shell init of a workspace: a script the workspace's login shells source, edited in the UI.
//! The host's copy is the only source of truth; the workspace sees a read-only copy.

use crate::error::StoreError;

/// The most bytes of a shell init. Every shell the workspace starts reads it, so a script this
/// long is already a mistake; it also fits the API's request body (64 KiB) with room for JSON.
pub const MAX_SHELL_INIT_BYTES: usize = 48 * 1024;

/// A shell init that is fit to store and to source: text with LF line ends, at most
/// [`MAX_SHELL_INIT_BYTES`], no NUL, and not empty (an empty script is no script).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInitText(String);

impl ShellInitText {
    /// Checks `text`. CRLF and lone CR line ends become LF (a browser's textarea can send
    /// either), because a CR at the end of a shell line is part of the word and breaks the line.
    /// The text ends in one newline.
    ///
    /// # Errors
    /// [`StoreError::ShellInitInvalid`] for an empty text, a NUL, or a text over the limit; the
    /// message says which.
    pub fn new(text: &str) -> Result<Self, StoreError> {
        if text.contains('\0') {
            return Err(invalid("a shell init cannot contain a NUL character"));
        }
        let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.trim().is_empty() {
            return Err(invalid(
                "the shell init is empty; remove it instead to start workspaces without one",
            ));
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if text.len() > MAX_SHELL_INIT_BYTES {
            return Err(invalid(format!(
                "a shell init is at most {} KiB; every shell the workspace starts reads it, so keep it short",
                MAX_SHELL_INIT_BYTES / 1024
            )));
        }
        Ok(Self(text))
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn invalid(what: impl Into<String>) -> StoreError {
    StoreError::ShellInitInvalid(what.into())
}

/// A workspace's stored shell init.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInit {
    /// The script.
    pub content: ShellInitText,
    /// When it was last saved, in milliseconds since the epoch.
    pub changed_at: u64,
}

#[cfg(test)]
mod tests {
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
}
