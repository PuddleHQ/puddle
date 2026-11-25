// SPDX-License-Identifier: AGPL-3.0-or-later
//! Why the output of a guest script could not be read.

/// What is wrong with the output of a script run in the guest. It displays as the message shown
/// to the user. A `line` is already cut short and made printable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ScriptOutputError {
    /// A line after the end marker.
    #[error("output after the end marker")]
    AfterEnd,
    /// More checkouts than the reader keeps.
    #[error("more than {max} checkouts")]
    TooManyCheckouts {
        /// The most it keeps.
        max: usize,
    },
    /// A count line whose count or kind is not valid.
    #[error("bad count line {line:?}")]
    BadCount {
        /// The line.
        line: String,
    },
    /// A line that fits no record.
    #[error("unexpected output line {line:?}")]
    UnexpectedLine {
        /// The line.
        line: String,
    },
    /// A record of kind `tag` before the checkout it belongs to.
    #[error("{tag} line before any checkout")]
    BeforeCheckout {
        /// The record's tag.
        tag: String,
    },
    /// The output ended with no end marker.
    #[error("{script} did not finish (no end marker)")]
    Unfinished {
        /// What ran, e.g. `the check`.
        script: &'static str,
    },
}
