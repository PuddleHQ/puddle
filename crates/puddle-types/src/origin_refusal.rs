// SPDX-License-Identifier: AGPL-3.0-or-later
//! Why a checkout's `origin` remote gives no repository address Puddle can use.

/// The words every refusal that names an address ends with.
const GIVE_HTTPS: &str = "give the repository's HTTPS address";

/// Why a checkout on this computer has no `origin` Puddle can use. It displays as the reason shown
/// beside the folder. An address in a variant is already redacted: it never holds a password.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum OriginRefusal {
    /// The `.git` folder has no `config` file.
    #[error("it has no config file, so no origin remote")]
    NoConfigFile,
    /// The config file could not be opened or read.
    #[error("its config can't be read: {cause}")]
    ConfigUnreadable {
        /// The operating system's words.
        cause: String,
    },
    /// The config is a link, a device or another thing that is not a plain file.
    #[error("its config is not a plain file")]
    ConfigNotPlain,
    /// The config is larger than the reader takes.
    #[error("its config file is too large to read")]
    ConfigTooLarge,
    /// The config holds text git could read differently from this reader.
    #[error(
        "its origin is written in a way Puddle does not read (a continued line, an include before \
         it, or an escape); give the repository's HTTPS address"
    )]
    Unreadable,
    /// The config has no `[remote "origin"]` with a `url`.
    #[error("it has no origin remote")]
    NoOrigin,
    /// The address holds white space or a control character.
    #[error("its origin {shown} holds spaces or control characters; {GIVE_HTTPS}")]
    ControlCharacters {
        /// The address, redacted.
        shown: String,
    },
    /// An `https://` address with no host or no path.
    #[error("its origin {shown} names no repository on a host; {GIVE_HTTPS}")]
    NoRepository {
        /// The address, redacted.
        shown: String,
    },
    /// The address is neither HTTPS nor SSH.
    #[error("its origin {shown} is not an HTTPS or SSH address; {GIVE_HTTPS}")]
    NotHttpsOrSsh {
        /// The address, redacted.
        shown: String,
    },
    /// An SSH address with a query, fragment or backslash in its path.
    #[error("its origin {shown} is an SSH address Puddle can't read; {GIVE_HTTPS}")]
    SshUnreadable {
        /// The address, redacted.
        shown: String,
    },
    /// An Azure DevOps SSH address that is not `v3/<org>/<project>/<repo>`.
    #[error("its origin {shown} is an Azure DevOps SSH address Puddle can't read; {GIVE_HTTPS}")]
    AzureUnreadable {
        /// The address, redacted.
        shown: String,
    },
    /// An SSH host Puddle knows no HTTPS form for.
    #[error(
        "its origin {shown} is an SSH address of a host Puddle knows no HTTPS form for; {GIVE_HTTPS}"
    )]
    UnknownHost {
        /// The address, redacted.
        shown: String,
    },
}
