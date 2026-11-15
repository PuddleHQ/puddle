// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validated names: workspace ids, sandboxes, volumes and image references.
//!
//! Workspace id, sandbox and volume names are DNS labels (RFC 1123: `a-z`, `0-9`, `-`, 1–63
//! characters, starting and ending with a letter or digit). That is stricter than msb's own rule
//! (it also allows upper case, `.` and `_`), so every Puddle name is a valid msb name, and a name
//! can also be used as a host label (`<name>.localhost`) and in a pipe or file name unchanged.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// Longest DNS label (RFC 1035 §2.3.4).
const MAX_LABEL_LEN: usize = 63;

/// Prefix of a workspace's volume name: workspace `abc` keeps its files in volume `ws-abc`
/// (ADR 0006).
pub const WORKSPACE_VOLUME_PREFIX: &str = "ws-";

/// Workspace ids Puddle refuses because the desktop shell uses them as host names
/// (`tauri.localhost`, `ipc.localhost`, `asset.localhost`).
pub const RESERVED_WORKSPACE_NAMES: [&str; 3] = ["tauri", "ipc", "asset"];

/// The alphabet of generated workspace ids: lowercase Crockford base32, digits first.
const CROCKFORD: &str = "0123456789abcdefghjkmnpqrstvwxyz";
/// How many of [`CROCKFORD`]'s symbols are digits; the rest are letters.
const DIGITS: usize = 10;
/// How many of [`CROCKFORD`]'s symbols are letters.
const LETTERS: usize = CROCKFORD.len() - DIGITS;

/// Checks `value` against the DNS-label rule with at most `max_len` characters.
fn check_label(what: &'static str, value: &str, max_len: usize) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::new(what, value, "must not be empty"));
    }
    if value.len() > max_len {
        return Err(ValidationError::new(
            what,
            value,
            format_args!("must be at most {max_len} characters"),
        ));
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(ValidationError::new(
            what,
            value,
            "may only contain a-z, 0-9 and '-'",
        ));
    }
    let edge_ok = |b: Option<u8>| b.is_some_and(|b| b != b'-');
    if !edge_ok(value.bytes().next()) || !edge_ok(value.bytes().last()) {
        return Err(ValidationError::new(
            what,
            value,
            "must start and end with a letter or digit",
        ));
    }
    Ok(())
}

/// Implements the string plumbing shared by the validated name types: `as_str`, `Display`,
/// `AsRef<str>`, `FromStr`, `TryFrom<String>`, `TryFrom<&str>` and `From<Name> for String`.
macro_rules! string_newtype {
    ($ty:ident) => {
        impl $ty {
            /// The name as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $ty {
            type Err = ValidationError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = ValidationError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                Self::new(&s)
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = ValidationError;

            fn try_from(s: &str) -> Result<Self, Self::Error> {
                Self::new(s)
            }
        }

        impl From<$ty> for String {
            fn from(n: $ty) -> String {
                n.0
            }
        }
    };
}

/// The name of a sandbox in the runtime (the microVM that runs a workspace): a DNS label.
///
/// A workspace's sandbox has the workspace's id ([`WorkspaceId::sandbox_name`]); Puddle's own
/// helper sandboxes (such as the one that checks a stopped workspace's volume) have names no
/// workspace can have, which is why this is a type of its own.
///
/// ```
/// use puddle_types::{SandboxName, WorkspaceId};
/// let w = WorkspaceId::new("my-project").unwrap();
/// assert_eq!(w.sandbox_name().as_str(), "my-project");
/// assert_eq!(w.sandbox_name().workspace_id(), Some(w));
/// assert!(SandboxName::new("m--my-project").is_ok());
/// assert!(SandboxName::new("My_Project").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        min_length = 1,
        max_length = 63,
        pattern = "^[a-z0-9]([a-z0-9-]*[a-z0-9])?$",
        example = "my-project",
        description = "A sandbox's name: a DNS label (`a-z`, `0-9`, `-`, no leading or trailing `-`)."
    )
)]
pub struct SandboxName(String);

impl SandboxName {
    /// Checks `name` and wraps it.
    ///
    /// # Errors
    ///
    /// When `name` isn't a DNS label (empty, longer than 63, characters other than `a-z`, `0-9`
    /// and `-`, or a leading/trailing `-`).
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        check_label("sandbox name", name, MAX_LABEL_LEN)?;
        Ok(Self(name.to_owned()))
    }

    /// The workspace this sandbox runs, or `None` when the name is not a valid workspace id or
    /// is one of [`RESERVED_WORKSPACE_NAMES`].
    #[must_use]
    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        WorkspaceId::new(&self.0).ok()
    }
}

string_newtype!(SandboxName);

/// The eight characters 40 random bits give (see [`WorkspaceId::from_random`]).
fn crockford_id(bytes: [u8; 5]) -> String {
    const LEN: usize = WorkspaceId::GENERATED_LEN;
    let [b0, b1, b2, b3, b4] = bytes;
    let bits = u64::from_be_bytes([0, 0, 0, b0, b1, b2, b3, b4]);
    (0..LEN)
        .map(|i| {
            let five = (bits >> (5 * (LEN - 1 - i))) & 0x1f;
            let five = usize::try_from(five).unwrap_or_default();
            let symbol = if i == 0 {
                DIGITS + five % LETTERS
            } else {
                five
            };
            CROCKFORD.chars().nth(symbol).unwrap_or('0')
        })
        .collect()
}

/// A workspace's identifier: a DNS label of at most 60 characters, so its volume name
/// (`ws-<id>`, see [`WorkspaceId::volume_name`]) is a DNS label too.
///
/// ```
/// use puddle_types::WorkspaceId;
/// let id = WorkspaceId::new("acme-api").unwrap();
/// assert_eq!(id.volume_name().as_str(), "ws-acme-api");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        min_length = 1,
        max_length = 60,
        pattern = "^[a-z0-9]([a-z0-9-]*[a-z0-9])?$",
        example = "my-project",
        description = "A workspace's id: a DNS label (`a-z`, `0-9`, `-`, no leading or trailing `-`) \
                       of at most 60 characters."
    )
)]
pub struct WorkspaceId(String);

impl WorkspaceId {
    /// Longest workspace id: 63 minus the length of [`WORKSPACE_VOLUME_PREFIX`].
    pub const MAX_LEN: usize = MAX_LABEL_LEN - WORKSPACE_VOLUME_PREFIX.len();

    /// Checks `id` and wraps it.
    ///
    /// # Errors
    ///
    /// When `id` isn't a DNS label of at most [`WorkspaceId::MAX_LEN`] characters, or is one of
    /// [`RESERVED_WORKSPACE_NAMES`] (the desktop shell's own origins).
    pub fn new(id: &str) -> Result<Self, ValidationError> {
        check_label("workspace id", id, Self::MAX_LEN)?;
        if RESERVED_WORKSPACE_NAMES.contains(&id) {
            return Err(ValidationError::new(
                "workspace id",
                id,
                "is reserved for the desktop shell",
            ));
        }
        Ok(Self(id.to_owned()))
    }

    /// Length of a generated id.
    pub const GENERATED_LEN: usize = 8;

    /// The id that 40 random bits give: eight characters of lowercase Crockford base32 (`0-9`
    /// and `a-z` without `i`, `l`, `o` and `u`), the first one a letter. Each character takes
    /// five bits; the first maps its five onto the 22 letters, so an id carries about 39 bits.
    /// It is a DNS label without a hyphen and longer than every reserved name.
    ///
    /// ```
    /// use puddle_types::WorkspaceId;
    /// assert_eq!(WorkspaceId::from_random([0; 5]).as_str(), "a0000000");
    /// assert_eq!(WorkspaceId::from_random([0xff; 5]).as_str(), "kzzzzzzz");
    /// ```
    #[must_use]
    pub fn from_random(bytes: [u8; 5]) -> Self {
        Self(crockford_id(bytes))
    }

    /// The name of the sandbox that runs this workspace: the same label.
    #[must_use]
    pub fn sandbox_name(&self) -> SandboxName {
        SandboxName(self.0.clone())
    }

    /// The name of the disk volume that holds this workspace's files: `ws-<id>` (ADR 0006).
    #[must_use]
    pub fn volume_name(&self) -> VolumeName {
        VolumeName(format!("{WORKSPACE_VOLUME_PREFIX}{}", self.0))
    }
}

string_newtype!(WorkspaceId);

/// The identifier of a repository entry on a workspace's list: what API paths and problem keys
/// use instead of the checkout's directory. It has the form of a generated workspace id
/// ([`WorkspaceId::from_random`]) and is unique within its workspace only.
///
/// ```
/// use puddle_types::EntryId;
/// assert_eq!(EntryId::from_random([0; 5]).as_str(), "a0000000");
/// assert!(EntryId::new("api").is_err());
/// assert!(EntryId::new("a0000000").is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        min_length = 8,
        max_length = 8,
        pattern = "^[a-hjkmnp-tv-z][0-9a-hjkmnp-tv-z]{7}$",
        example = "a0000000",
        description = "A repository entry's id: eight characters of lowercase Crockford base32, \
                       the first a letter. Unique within its workspace."
    )
)]
pub struct EntryId(String);

impl EntryId {
    /// Checks `id` and wraps it.
    ///
    /// # Errors
    ///
    /// When `id` is not eight Crockford base32 characters (`0-9`, `a-z` without `i`, `l`, `o`
    /// and `u`) with a letter first: what [`EntryId::from_random`] gives.
    pub fn new(id: &str) -> Result<Self, ValidationError> {
        let symbol = |b: u8| CROCKFORD.bytes().any(|c| c == b);
        let well_formed = id.len() == WorkspaceId::GENERATED_LEN
            && id.bytes().all(symbol)
            && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase());
        if well_formed {
            Ok(Self(id.to_owned()))
        } else {
            Err(ValidationError::new(
                "entry id",
                id,
                "must be eight characters of 0-9 and a-z without i, l, o and u, the first a letter",
            ))
        }
    }

    /// The entry id that 40 random bits give, the same way [`WorkspaceId::from_random`] does.
    #[must_use]
    pub fn from_random(bytes: [u8; 5]) -> Self {
        Self(crockford_id(bytes))
    }
}

string_newtype!(EntryId);

/// A named disk volume's name: a DNS label.
///
/// ```
/// use puddle_types::{VolumeName, WorkspaceId};
/// let v = VolumeName::new("ws-acme-api").unwrap();
/// assert_eq!(v.workspace_id(), Some(WorkspaceId::new("acme-api").unwrap()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct VolumeName(String);

impl VolumeName {
    /// Checks `name` and wraps it.
    ///
    /// # Errors
    ///
    /// When `name` isn't a DNS label.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        check_label("volume name", name, MAX_LABEL_LEN)?;
        Ok(Self(name.to_owned()))
    }

    /// The workspace this volume belongs to, if its name has the `ws-` prefix.
    #[must_use]
    pub fn workspace_id(&self) -> Option<WorkspaceId> {
        self.0
            .strip_prefix(WORKSPACE_VOLUME_PREFIX)
            .and_then(|id| WorkspaceId::new(id).ok())
    }
}

string_newtype!(VolumeName);

/// An OCI image reference as the user wrote it (`mcr.microsoft.com/devcontainers/base:debian`).
///
/// Only checked for shape (non-empty, at most 512 characters, printable ASCII without spaces);
/// the runtime resolves and rejects references it can't pull.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ImageRef(String);

impl ImageRef {
    /// Longest accepted reference.
    pub const MAX_LEN: usize = 512;

    /// Checks `reference` and wraps it.
    ///
    /// # Errors
    ///
    /// When `reference` is empty, too long, or contains whitespace or non-printable characters.
    pub fn new(reference: &str) -> Result<Self, ValidationError> {
        if reference.is_empty() {
            return Err(ValidationError::new(
                "image reference",
                reference,
                "must not be empty",
            ));
        }
        if reference.len() > Self::MAX_LEN {
            return Err(ValidationError::new(
                "image reference",
                reference,
                format_args!("must be at most {} characters", Self::MAX_LEN),
            ));
        }
        if !reference.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ValidationError::new(
                "image reference",
                reference,
                "may only contain printable ASCII without spaces",
            ));
        }
        Ok(Self(reference.to_owned()))
    }
}

string_newtype!(ImageRef);

/// An image reference written in the source and checked when the crate is built.
///
/// Use [`image_ref!`](crate::image_ref) for a constant; a reference from input goes through
/// [`ImageRef::new`].
///
/// ```
/// use puddle_types::{ImageRef, image_ref};
/// let image: ImageRef = image_ref!("mcr.microsoft.com/devcontainers/base:debian");
/// assert_eq!(image.as_str(), "mcr.microsoft.com/devcontainers/base:debian");
/// ```
///
/// ```compile_fail
/// use puddle_types::image_ref;
/// let _ = image_ref!("has space");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRefLit(&'static str);

impl ImageRefLit {
    /// Checks `reference`.
    ///
    /// # Panics
    ///
    /// When `reference` is not a valid [`ImageRef`]. In a `const` context that is a build error.
    #[must_use]
    pub const fn new(reference: &'static str) -> Self {
        assert!(
            !reference.is_empty() && reference.len() <= ImageRef::MAX_LEN,
            "an image reference is 1 to 512 characters"
        );
        let mut rest = reference.as_bytes();
        while let [byte, tail @ ..] = rest {
            assert!(
                byte.is_ascii_graphic(),
                "an image reference is printable ASCII without spaces"
            );
            rest = tail;
        }
        Self(reference)
    }
}

impl From<ImageRefLit> for ImageRef {
    fn from(lit: ImageRefLit) -> Self {
        Self(lit.0.to_owned())
    }
}

/// An [`ImageRef`] from a constant string, checked at build time: no `Result`, no `expect`.
#[macro_export]
macro_rules! image_ref {
    ($reference:expr) => {
        $crate::ImageRef::from(const { $crate::ImageRefLit::new($reference) })
    };
}

/// A checkout's directory directly under a workspace's volume root: ASCII letters, digits, `.`,
/// `_` and `-`, not starting with `.` or `-`, at most [`CheckoutDir::MAX_LEN`] characters, and
/// not `lost+found`. So it is one path segment that never names Puddle's own `.puddle` folder.
///
/// ```
/// use puddle_types::CheckoutDir;
/// assert_eq!(CheckoutDir::from_url("https://github.com/acme/api.git").as_str(), "api");
/// assert_eq!(CheckoutDir::from_url("git@github.com:acme/web").as_str(), "web");
/// assert_eq!(CheckoutDir::from_url("https://example.org/").as_str(), "example.org");
/// assert_eq!(CheckoutDir::from_url("https://h/x/..git").as_str(), "src");
/// assert!(CheckoutDir::new(".puddle").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        min_length = 1,
        max_length = 100,
        pattern = "^[A-Za-z0-9_][A-Za-z0-9._-]*$",
        example = "api",
        description = "A checkout's directory directly under the workspace's volume root: ASCII \
                       letters, digits, `.`, `_` and `-`, not starting with `.` or `-`, not `lost+found`."
    )
)]
pub struct CheckoutDir(String);

impl CheckoutDir {
    /// Longest directory name.
    pub const MAX_LEN: usize = 100;

    /// The directory a URL whose last part is no usable name is cloned into.
    pub const FALLBACK: &str = "src";

    /// The folder `mkfs` may make in a volume's root; never a checkout.
    pub const LOST_AND_FOUND: &str = "lost+found";

    /// Checks `name` and wraps it.
    ///
    /// # Errors
    ///
    /// When `name` breaks one of the rules above.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        let reason = if name.is_empty() {
            "must not be empty"
        } else if name.len() > Self::MAX_LEN {
            "must be at most 100 characters"
        } else if name.starts_with(['.', '-']) {
            "must not start with `.` or `-`"
        } else if name == Self::LOST_AND_FOUND {
            "is the volume's own lost+found folder"
        } else if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            "may only contain ASCII letters, digits, `.`, `_` and `-`"
        } else {
            return Ok(Self(name.to_owned()));
        };
        Err(ValidationError::new("checkout directory", name, reason))
    }

    /// The directory `git clone <url>` would make: the last part of the URL without `.git`, or
    /// [`CheckoutDir::FALLBACK`] when that is no usable name.
    #[must_use]
    pub fn from_url(url: &str) -> Self {
        let trimmed = url.trim_end_matches('/');
        let last = trimmed.rsplit(['/', ':']).next().unwrap_or_default();
        let name = last.strip_suffix(".git").unwrap_or(last);
        Self::new(name).unwrap_or_else(|_| Self(Self::FALLBACK.to_owned()))
    }
}

string_newtype!(CheckoutDir);

#[cfg(test)]
mod tests;
