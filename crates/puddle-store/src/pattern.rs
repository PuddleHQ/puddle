// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rule patterns: an exact host, or a `.example.com` suffix (R-2 to R-4).

use std::fmt;

use puddle_types::{DomainName, Host, PatternKind, ValidationError};

/// Why a pattern was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    /// The host part is not a normalised name or IP literal.
    #[error("pattern host is invalid: {0}")]
    Host(#[from] ValidationError),
    /// A suffix pattern was given for an IP literal.
    #[error("a suffix pattern needs a name, not an ip literal")]
    SuffixOfIp,
    /// The suffix is a public suffix, or shorter (R-4).
    #[error("suffix .{0} is a public suffix; use a longer one")]
    PublicSuffix(String),
    /// The suffix is not a suffix of the pending row's host (R-15).
    #[error("suffix .{suffix} does not cover host {host}")]
    NotASuffixOf {
        /// The offered suffix, without the leading dot.
        suffix: String,
        /// The row's host.
        host: String,
    },
}

/// A suffix pattern: matches names strictly below `base`, never `base` itself (R-3).
///
/// Stored and displayed as `.base`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SuffixPattern(DomainName);

impl SuffixPattern {
    /// Builds the suffix `.base`, refusing public suffixes and anything shorter (R-4).
    ///
    /// # Errors
    /// [`PatternError::PublicSuffix`] when `base` has no registrable domain.
    pub fn new(base: DomainName) -> Result<Self, PatternError> {
        if psl::domain(base.as_str().as_bytes()).is_none() {
            return Err(PatternError::PublicSuffix(base.as_str().to_owned()));
        }
        Ok(Self(base))
    }

    /// A suffix read back from the database, trusted without the public-suffix check so that a
    /// list update can't make stored rules unreadable.
    pub(crate) fn from_stored(base: DomainName) -> Self {
        Self(base)
    }

    /// The name below which this suffix matches, without the leading dot.
    #[must_use]
    pub fn base(&self) -> &DomainName {
        &self.0
    }
}

impl fmt::Display for SuffixPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".{}", self.0)
    }
}

/// What a rule matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// The identical normalised host or IP literal, on any port (R-2).
    Exact(Host),
    /// Every name below a suffix (R-3).
    Suffix(SuffixPattern),
}

impl Pattern {
    /// Parses user input: `host`, `.example.com` or `*.example.com` (stored as `.example.com`).
    ///
    /// ```
    /// use puddle_store::Pattern;
    /// assert_eq!(Pattern::parse("*.example.com").unwrap().to_string(), ".example.com");
    /// assert_eq!(Pattern::parse("api.example.com").unwrap().to_string(), "api.example.com");
    /// assert!(Pattern::parse(".co.uk").is_err());
    /// ```
    ///
    /// # Errors
    /// A [`PatternError`] for an invalid host or a too-broad suffix.
    pub fn parse(input: &str) -> Result<Self, PatternError> {
        let base = input.strip_prefix("*.").or_else(|| input.strip_prefix('.'));
        match base {
            Some(base) => match Host::parse_normalised(base)? {
                Host::Name(name) => SuffixPattern::new(name).map(Self::Suffix),
                Host::Ip(_) => Err(PatternError::SuffixOfIp),
            },
            None => Ok(Self::Exact(Host::parse_normalised(input)?)),
        }
    }

    /// A suffix pattern covering `host`, for approving a pending row with a wider pattern
    /// (R-15). `suffix` is input as for [`Pattern::parse`] and must cover `host` strictly.
    ///
    /// # Errors
    /// [`PatternError::NotASuffixOf`] when it doesn't, plus the errors of [`Pattern::parse`].
    pub fn suffix_covering(host: &Host, suffix: &str) -> Result<Self, PatternError> {
        let Host::Name(name) = host else {
            return Err(PatternError::SuffixOfIp);
        };
        let bare = suffix
            .strip_prefix("*.")
            .or_else(|| suffix.strip_prefix('.'))
            .unwrap_or(suffix);
        let pattern = Self::parse(&format!(".{bare}"))?;
        if pattern.matches(host) {
            Ok(pattern)
        } else {
            Err(PatternError::NotASuffixOf {
                suffix: bare.to_owned(),
                host: name.to_string(),
            })
        }
    }

    /// Whether this pattern matches `host`.
    #[must_use]
    pub fn matches(&self, host: &Host) -> bool {
        match (self, host) {
            (Self::Exact(want), host) => want == host,
            (Self::Suffix(suffix), Host::Name(name)) => {
                let base = suffix.base().as_str();
                name.as_str()
                    .strip_suffix(base)
                    .is_some_and(|head| head.ends_with('.'))
            }
            (Self::Suffix(_), Host::Ip(_)) => false,
        }
    }

    /// Exact or suffix.
    #[must_use]
    pub fn kind(&self) -> PatternKind {
        match self {
            Self::Exact(_) => PatternKind::Exact,
            Self::Suffix(_) => PatternKind::Suffix,
        }
    }

    /// The precedence key of R-6 step 1: exact beats any suffix, a longer suffix (more labels)
    /// beats a shorter one.
    #[must_use]
    pub(crate) fn specificity(&self) -> (u8, usize) {
        match self {
            Self::Exact(_) => (1, 0),
            Self::Suffix(suffix) => (0, suffix.base().labels().count()),
        }
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(host) => host.fmt(f),
            Self::Suffix(suffix) => suffix.fmt(f),
        }
    }
}

/// The registrable domain of `host` for grouping the inbox (R-18): `example.co.uk` for
/// `a.b.example.co.uk`; the host itself for IP literals and public suffixes.
#[must_use]
pub fn registrable_domain(host: &Host) -> String {
    match host {
        Host::Name(name) => {
            psl::domain_str(name.as_str()).map_or_else(|| name.to_string(), str::to_owned)
        }
        Host::Ip(ip) => ip.to_string(),
    }
}

#[cfg(test)]
mod tests;
