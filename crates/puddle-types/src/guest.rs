// SPDX-License-Identifier: GPL-3.0-or-later
//! What Puddle puts into a guest: paths, files and environment variables.
//!
//! Providers (proxy config, corporate roots, the per-workspace CA) produce [`GuestFile`]s and
//! [`GuestEnv`]; the boot hook applies them as a list and holds no provider logic itself.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValidationError;
use crate::merge::MergeSpec;

/// Longest accepted guest path (Linux `PATH_MAX`).
const MAX_PATH_LEN: usize = 4096;

/// Whether `path` is a valid [`GuestPath`], as a `const fn` so a literal is checked at compile time
/// (see [`GuestPathLit`]). [`GuestPath::new`] agrees with it (a unit test compares them).
const fn is_valid_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let [b'/', after_root @ ..] = bytes else {
        return false;
    };
    if bytes.len() > MAX_PATH_LEN {
        return false;
    }
    if after_root.is_empty() {
        return true;
    }
    // A component is valid unless it is empty, `.` or `..`.
    let mut len = 0_usize;
    let mut only_dots = true;
    let mut rest = after_root;
    while let [byte, tail @ ..] = rest {
        rest = tail;
        match *byte {
            0 => return false,
            b'/' => {
                if len == 0 || (only_dots && len <= 2) {
                    return false;
                }
                len = 0;
                only_dots = true;
            }
            other => {
                len += 1;
                if other != b'.' {
                    only_dots = false;
                }
            }
        }
    }
    len != 0 && !(only_dots && len <= 2)
}

/// A guest path written in the source and checked when the crate is built, not when it runs.
///
/// Use [`guest_path!`](crate::guest_path) for a constant; a path that comes from input goes
/// through [`GuestPath::new`], which returns the error.
///
/// ```
/// use puddle_types::{GuestPath, guest_path};
/// const AGENT: &str = "/opt/puddle/agent";
/// let p: GuestPath = guest_path!(AGENT);
/// assert_eq!(p.as_str(), "/opt/puddle/agent");
/// ```
///
/// A relative one does not compile:
///
/// ```compile_fail
/// use puddle_types::guest_path;
/// let _ = guest_path!("relative");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestPathLit(&'static str);

impl GuestPathLit {
    /// Checks `path`.
    ///
    /// # Panics
    ///
    /// When `path` is not a valid [`GuestPath`]. In a `const` context that is a build error.
    #[must_use]
    pub const fn new(path: &'static str) -> Self {
        assert!(is_valid_path(path), "not a valid guest path");
        Self(path)
    }
}

impl From<GuestPathLit> for GuestPath {
    fn from(lit: GuestPathLit) -> Self {
        Self(lit.0.to_owned())
    }
}

/// A [`GuestPath`] from a constant string, checked at build time: no `Result`, no `expect`.
///
/// The argument must be a literal or a `const`; the check runs in a `const` block, so an invalid
/// path stops the build.
#[macro_export]
macro_rules! guest_path {
    ($path:expr) => {
        $crate::GuestPath::from(const { $crate::GuestPathLit::new($path) })
    };
}

/// An absolute, normalised path inside the guest (always `/`-separated, whatever the host OS).
///
/// Normalised means: starts with `/`, no empty, `.` or `..` components, no trailing `/` (except
/// the root itself), no NUL. So two `GuestPath`s name the same file exactly when they are equal,
/// and a path can't climb out of a mount with `..`.
///
/// ```
/// use puddle_types::GuestPath;
/// let p = GuestPath::new("/workspaces/acme/src").unwrap();
/// assert!(p.is_within(&GuestPath::new("/workspaces/acme").unwrap()));
/// assert!(GuestPath::new("/workspaces/../etc").is_err());
/// assert!(GuestPath::new("relative").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GuestPath(String);

impl GuestPath {
    /// Checks `path` and wraps it.
    ///
    /// # Errors
    ///
    /// When `path` is relative, not normalised (empty, `.` or `..` components, trailing `/`),
    /// contains NUL, or is longer than 4096 bytes.
    pub fn new(path: &str) -> Result<Self, ValidationError> {
        const WHAT: &str = "guest path";
        if !path.starts_with('/') {
            return Err(ValidationError::new(WHAT, path, "must be absolute"));
        }
        if path.len() > MAX_PATH_LEN {
            return Err(ValidationError::new(
                WHAT,
                path,
                format_args!("must be at most {MAX_PATH_LEN} bytes"),
            ));
        }
        if path.contains('\0') {
            return Err(ValidationError::new(WHAT, path, "must not contain NUL"));
        }
        if path != "/" {
            for component in path.split('/').skip(1) {
                if component.is_empty() || component == "." || component == ".." {
                    return Err(ValidationError::new(
                        WHAT,
                        path,
                        "must be normalised (no empty, '.' or '..' components, no trailing '/')",
                    ));
                }
            }
        }
        Ok(Self(path.to_owned()))
    }

    /// The path as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this path is `dir` itself or lies below it (component-wise, so `/ab` is not within
    /// `/a`).
    #[must_use]
    pub fn is_within(&self, dir: &GuestPath) -> bool {
        self.relative_to(dir).is_some()
    }

    /// This path relative to `dir` (`""` for `dir` itself), or `None` if it isn't within `dir`.
    #[must_use]
    pub fn relative_to(&self, dir: &GuestPath) -> Option<&str> {
        if dir.0 == "/" {
            return Some(self.0.trim_start_matches('/'));
        }
        let rest = self.0.strip_prefix(dir.0.as_str())?;
        if rest.is_empty() {
            Some("")
        } else {
            rest.strip_prefix('/')
        }
    }
}

impl fmt::Display for GuestPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for GuestPath {
    type Error = ValidationError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl TryFrom<&str> for GuestPath {
    type Error = ValidationError;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<GuestPath> for String {
    fn from(p: GuestPath) -> String {
        p.0
    }
}

/// Permission bits of a [`GuestFile`]: within `0o7777`.
///
/// ```
/// use puddle_types::FileMode;
/// const SECRET: FileMode = FileMode::new(0o600);
/// assert_eq!(SECRET.bits(), 0o600);
/// assert!(FileMode::try_new(0o10000).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMode(u32);

impl FileMode {
    /// Checks `mode`, in a `const` it stops the build.
    ///
    /// # Panics
    ///
    /// When `mode` has bits outside `0o7777`.
    #[must_use]
    pub const fn new(mode: u32) -> Self {
        assert!(mode & !0o7777 == 0, "a file mode is within 0o7777");
        Self(mode)
    }

    /// Checks a mode that comes from input.
    ///
    /// # Errors
    ///
    /// When `mode` has bits outside `0o7777`.
    pub fn try_new(mode: u32) -> Result<Self, ValidationError> {
        if mode & !0o7777 != 0 {
            return Err(ValidationError::new(
                "file mode",
                &format!("{mode:o}"),
                "must be within 0o7777",
            ));
        }
        Ok(Self(mode))
    }

    /// The permission bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

/// A file the boot hook writes into the guest: path, contents, permission bits and how it is
/// applied ([`ApplyKind`]).
///
/// Contents are public material (configs, CA certificates) unless the mode leaves group and
/// others no access (`0o600`): then they are a secret meant for the guest (a private key), and
/// `Debug` prints the length instead of the bytes. A secret file is copied into the guest's root
/// disk like the rest, so only put one here when the guest has to hold it.
///
/// ```
/// use puddle_types::{FileMode, GuestFile, GuestPath};
/// let f = GuestFile::new(GuestPath::new("/etc/profile.d/01-puddle-env.sh").unwrap(), b"export A=1\n".to_vec())
///     .with_mode(FileMode::new(0o644));
/// assert_eq!(f.mode(), 0o644);
/// ```
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestFile {
    path: GuestPath,
    contents: Vec<u8>,
    mode: u32,
    #[serde(default)]
    apply: ApplyKind,
}

impl fmt::Debug for GuestFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = f.debug_struct("GuestFile");
        shown.field("path", &self.path);
        // No bit set for group or others: an owner-only secret.
        if self.mode.trailing_zeros() >= 6 {
            shown.field(
                "contents",
                &format_args!("<{} bytes, owner-only>", self.contents.len()),
            );
        } else {
            shown.field("contents", &self.contents);
        }
        shown
            .field("mode", &self.mode)
            .field("apply", &self.apply)
            .finish()
    }
}

/// How the boot hook applies a [`GuestFile`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApplyKind {
    /// Puddle owns the whole file: written atomically at every boot, replacing what is there,
    /// and deleted when no provider lists it any more.
    #[default]
    Replace,
    /// The file is the user's; Puddle owns only the keys in the spec. They are set at
    /// every boot and removed when no provider lists the file any more; everything else stays.
    /// A file that can't be parsed is left alone. The file's mode applies when Puddle creates it.
    Merge(MergeSpec),
    /// A merged file an earlier plan wrote stays as it is: its keys are neither set nor removed,
    /// and they are still Puddle's to remove later. For a file only a running guest can work out,
    /// written again once it runs. Nothing happens when no earlier plan merged it.
    Keep,
}

impl GuestFile {
    /// Default permission bits: `0o644`.
    pub const DEFAULT_MODE: u32 = 0o644;

    /// A file at `path` with `contents` and mode [`GuestFile::DEFAULT_MODE`].
    #[must_use]
    pub fn new(path: GuestPath, contents: Vec<u8>) -> Self {
        Self {
            path,
            contents,
            mode: Self::DEFAULT_MODE,
            apply: ApplyKind::Replace,
        }
    }

    /// A merged file at `path`: Puddle owns only the keys in `spec` (see [`ApplyKind::Merge`]).
    /// [`GuestFile::contents`] is the file as Puddle writes it when none exists.
    ///
    /// ```
    /// use puddle_types::{ApplyKind, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec};
    /// let spec = MergeSpec::new(MergeFormat::Json, vec![MergeEntry::json(&["a"], &serde_json::json!(1))]).unwrap();
    /// let f = GuestFile::merged(GuestPath::new("/root/.docker/config.json").unwrap(), spec);
    /// assert_eq!(f.contents(), b"{\n\t\"a\": 1\n}\n");
    /// assert!(matches!(f.apply(), ApplyKind::Merge(_)));
    /// ```
    #[must_use]
    pub fn merged(path: GuestPath, spec: MergeSpec) -> Self {
        Self {
            path,
            contents: spec.fresh(),
            mode: Self::DEFAULT_MODE,
            apply: ApplyKind::Merge(spec),
        }
    }

    /// The merged file at `path` an earlier plan wrote, kept as it is (see [`ApplyKind::Keep`]).
    ///
    /// ```
    /// use puddle_types::{ApplyKind, GuestFile, GuestPath};
    /// let f = GuestFile::kept(GuestPath::new("/w/.puddle/editor/w.code-workspace").unwrap());
    /// assert!(matches!(f.apply(), ApplyKind::Keep));
    /// ```
    #[must_use]
    pub fn kept(path: GuestPath) -> Self {
        Self {
            path,
            contents: Vec::new(),
            mode: Self::DEFAULT_MODE,
            apply: ApplyKind::Keep,
        }
    }

    /// The same file with permission bits `mode`.
    #[must_use]
    pub fn with_mode(mut self, mode: FileMode) -> Self {
        self.mode = mode.bits();
        self
    }

    /// Where the file goes in the guest.
    #[must_use]
    pub fn path(&self) -> &GuestPath {
        &self.path
    }

    /// The file's contents.
    #[must_use]
    pub fn contents(&self) -> &[u8] {
        &self.contents
    }

    /// The file's permission bits.
    #[must_use]
    pub fn mode(&self) -> u32 {
        self.mode
    }

    /// How the boot hook applies the file.
    #[must_use]
    pub fn apply(&self) -> &ApplyKind {
        &self.apply
    }
}

/// Environment variables for the guest, by name. Later [`GuestEnv::set`]s and
/// [`GuestEnv::extend`]s win.
///
/// Names follow POSIX shell rules (`[A-Za-z_][A-Za-z0-9_]*`), values may be anything but NUL.
/// Case matters: `HTTP_PROXY` and `http_proxy` are separate variables (both are set).
///
/// ```
/// use puddle_types::GuestEnv;
/// let mut env = GuestEnv::new();
/// env.set("HTTPS_PROXY", "http://127.0.0.1:3128").unwrap();
/// env.set("https_proxy", "http://127.0.0.1:3128").unwrap();
/// assert_eq!(env.get("HTTPS_PROXY"), Some("http://127.0.0.1:3128"));
/// assert_eq!(env.len(), 2);
/// assert!(env.set("BAD-NAME", "x").is_err());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    try_from = "BTreeMap<String, String>",
    into = "BTreeMap<String, String>"
)]
pub struct GuestEnv(BTreeMap<String, String>);

impl GuestEnv {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `name` to `value`, replacing an earlier value.
    ///
    /// # Errors
    ///
    /// When `name` isn't a shell variable name or `value` contains NUL.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), ValidationError> {
        check_env_name(name)?;
        if value.contains('\0') {
            return Err(ValidationError::new(
                "environment value",
                name,
                "must not contain NUL",
            ));
        }
        self.0.insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    /// The value of `name`, if set.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// Adds every variable of `other`; `other` wins on equal names.
    pub fn extend(&mut self, other: &GuestEnv) {
        self.0
            .extend(other.0.iter().map(|(k, v)| (k.clone(), v.clone())));
    }

    /// The variables, sorted by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// How many variables are set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no variable is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn check_env_name(name: &str) -> Result<(), ValidationError> {
    let mut bytes = name.bytes();
    let first_ok = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_');
    if !first_ok || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(ValidationError::new(
            "environment variable name",
            name,
            "must match [A-Za-z_][A-Za-z0-9_]*",
        ));
    }
    Ok(())
}

impl TryFrom<BTreeMap<String, String>> for GuestEnv {
    type Error = ValidationError;

    fn try_from(map: BTreeMap<String, String>) -> Result<Self, Self::Error> {
        let mut env = GuestEnv::new();
        for (k, v) in &map {
            env.set(k, v)?;
        }
        Ok(env)
    }
}

impl From<GuestEnv> for BTreeMap<String, String> {
    fn from(env: GuestEnv) -> Self {
        env.0
    }
}

#[cfg(test)]
mod tests;
