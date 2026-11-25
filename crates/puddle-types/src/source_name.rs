// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validated names that end up as an argument or a line of a tool's input.
//!
//! A name that reaches a child process is checked first, so it cannot be an option (`-x`), carry a
//! newline into the Git credential protocol or a space into an argument list.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A value was not accepted as a name for a source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a valid {what}")]
pub struct NameError {
    what: &'static str,
}

macro_rules! name_type {
    ($(#[$doc:meta])* $name:ident, $what:literal, $ok:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// Checks `value` and wraps it.
            ///
            /// # Errors
            /// [`NameError`] when it is empty, too long, starts with `-` or has a character a
            /// tool argument or credential line must not carry.
            pub fn new(value: impl Into<String>) -> Result<Self, NameError> {
                let value = value.into();
                let check: fn(&str) -> bool = $ok;
                if value.is_empty() || value.len() > 256 || value.starts_with('-') || !check(&value) {
                    return Err(NameError { what: $what });
                }
                Ok(Self(value))
            }

            /// The text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = NameError;
            fn try_from(value: String) -> Result<Self, NameError> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> String {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

fn account_chars(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '+'))
}

fn host_chars(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':'))
        && !s.starts_with('.')
}

/// [`HostName::new`]'s rule as a `const fn`, so a literal is checked at build time. A unit test
/// compares it with the runtime rule.
const fn is_valid_host(value: &str) -> bool {
    let bytes = value.as_bytes();
    let [first, ..] = bytes else {
        return false;
    };
    if bytes.len() > 256 || *first == b'-' || *first == b'.' {
        return false;
    }
    let mut rest = bytes;
    while let [byte, tail @ ..] = rest {
        if !(byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b':')) {
            return false;
        }
        rest = tail;
    }
    true
}

fn path_chars(s: &str) -> bool {
    s.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '%' | '@' | '+' | '/')
    }) && !s.starts_with('/')
        && !s.ends_with('/')
        && s.split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

fn tenant_chars(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        && !s.starts_with('.')
}

fn id_chars(s: &str) -> bool {
    s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

name_type!(
    /// An account name on a Git host or Azure DevOps (`TijsVK`, `me@example.com`).
    AccountName, "account name", account_chars
);
name_type!(
    /// A Git host name (`github.com`, `dev.azure.com`), with an optional port.
    HostName, "host name", host_chars
);

/// A host name written in the source and checked when the crate is built.
///
/// Use [`host_name!`](crate::host_name) for a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostNameLit(&'static str);

impl HostNameLit {
    /// Checks `value`.
    ///
    /// # Panics
    ///
    /// When `value` is not a valid [`HostName`]. In a `const` context that is a build error.
    #[must_use]
    pub const fn new(value: &'static str) -> Self {
        assert!(is_valid_host(value), "not a valid host name");
        Self(value)
    }
}

impl From<HostNameLit> for HostName {
    fn from(lit: HostNameLit) -> Self {
        Self(lit.0.to_owned())
    }
}

/// A [`HostName`] from a constant string, checked at build time: no `Result`, no `expect`.
///
/// ```
/// use puddle_secrets::{HostName, host_name};
/// let host: HostName = host_name!("github.com");
/// assert_eq!(host.as_str(), "github.com");
/// ```
///
/// ```compile_fail
/// use puddle_secrets::host_name;
/// let _ = host_name!("-option");
/// ```
#[macro_export]
macro_rules! host_name {
    ($value:expr) => {
        $crate::HostName::from(const { $crate::HostNameLit::new($value) })
    };
}

name_type!(
    /// The path of a repository URL without the host (`org`, `org/project/_git/repo`): it selects
    /// the credential, so it is never empty.
    UrlPath, "URL path", path_chars
);
name_type!(
    /// An Entra tenant: a GUID or a domain name. Letters, digits, `.` and `-` only, so it is no
    /// option and carries nothing a Windows `az.cmd` batch file would re-read (`%`, `^`, `&`, `"`,
    /// space).
    TenantId, "tenant", tenant_chars
);
name_type!(
    /// The id a pasted token is stored under (`puddle:credential:<id>`).
    StoredId, "stored credential id", id_chars
);
name_type!(
    /// An organisation on a host (an Azure DevOps organisation).
    OrgName, "organisation name", account_chars
);

#[cfg(test)]
mod tests;
