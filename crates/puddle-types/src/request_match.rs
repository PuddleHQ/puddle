// SPDX-License-Identifier: GPL-3.0-or-later
//! What a rule on method and path compares: a canonical form of the request path, a path
//! pattern, and a set of methods (`docs/spec/rules.md` R-49..R-55).
//!
//! A request rule only ever denies, so the canonical form errs towards matching more: it
//! decodes, folds and collapses every spelling that some server treats as the same path, and
//! a guest that spells `/admin` as `/%61dmin`, `//admin/`, `/x/../ADMIN` or `/admin;a=b` still
//! meets the rule. Two paths that differ only in such spellings are the same path here even
//! where a given server would tell them apart; that costs a request a refusal, never a bypass.
//!
//! Where servers disagree about a spelling (is `%2f` inside a segment a separator, does `..`
//! count an empty segment, is `\` part of a name), one canonical form cannot over-match every
//! server at once. [`CanonicalPath::readings`] therefore lists each reading a server may make of
//! a request target, and a rule refuses the request when it names any of them.

use std::fmt;

/// Longest pattern text, in bytes.
pub const MAX_PATH_PATTERN: usize = 512;

/// Most segments a pattern has.
pub const MAX_PATH_SEGMENTS: usize = 32;

/// Most percent-decoding rounds: `%252e` is `%2e` after one and `.` after two. A path that is
/// still changing after this many is matched in its last form.
const DECODE_ROUNDS: usize = 4;

/// The path of a request target, as the server will read it: origin form (`/a/b?q`), absolute
/// form (`http://host/a/b?q`) or `*`. Query and fragment are cut; nothing is decoded.
///
/// A `://` inside an origin-form path is part of the path, not a scheme:
///
/// ```
/// use puddle_types::target_path;
/// assert_eq!(target_path("/a://b/c?q=1"), "/a://b/c");
/// assert_eq!(target_path("https://h.example/x#f"), "/x");
/// assert_eq!(target_path("http://h.example"), "/");
/// ```
#[must_use]
pub fn target_path(target: &str) -> &str {
    crate::request_path(target)
}

/// The path as segments, in canonical form (see the module text). Equal paths have equal
/// segments; `/` has none.
///
/// ```
/// use puddle_types::CanonicalPath;
/// let path = |p: &str| CanonicalPath::new(p).segments().join("/");
/// assert_eq!(path("/Admin/"), "admin");
/// assert_eq!(path("/%61dmin"), "admin");
/// assert_eq!(path("//a/./b/../admin"), "a/admin");
/// assert_eq!(path("/a/%2e%2e/admin"), "admin");
/// assert_eq!(path("/a%2f..%5cadmin"), "admin");
/// assert_eq!(path("/%252e%252e/admin"), "admin");
/// assert_eq!(path("/admin;jsessionid=1/x"), "admin/x");
/// assert_eq!(path("/admin./"), "admin");
/// assert_eq!(path("/admin%00.png"), "admin");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalPath {
    segments: Vec<String>,
}

impl CanonicalPath {
    /// The canonical form of `path`, a path without query or fragment ([`target_path`]).
    #[must_use]
    pub fn new(path: &str) -> Self {
        Self {
            segments: canonical_segments(path),
        }
    }

    /// The segments, lower case, with no empty, `.` or `..` among them and no control
    /// character in them. In the form [`CanonicalPath::new`] makes, no segment holds `/`, `\`
    /// or `;`; a reading from [`CanonicalPath::readings`] may keep an escaped `/`, a `\` or a
    /// segment's `;parameters` in a name.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Every way a server may read the path of the request target `target` (origin form, with
    /// its query), each in canonical form. A rule refuses the request when it names any of
    /// them, because servers disagree and a rule only refuses.
    ///
    /// The texts read are the path itself and, where they differ:
    ///
    /// - the path cut at a `?` or `#` that only decoding reveals, as a hop that decodes the path
    ///   before passing it on makes it: `/admin%3fx` is then `/admin`;
    /// - the path with a `#` read as a plain character (a server that keeps it reads
    ///   `/public#/../admin` as `/admin`);
    /// - the path without a leading `//name`, which a parser that resolves the target against a
    ///   base URL takes as an authority (`//evil.example/admin` is `/admin` there).
    ///
    /// Each text is read in every combination of these choices, because servers combine them in
    /// every way: decode the whole path and then split it, or split where it has a `/` or `\`
    /// as written and decode each segment (a router that matches undecoded segments reads
    /// `/repos/a%2fb` as `repos` and `a/b`); keep or drop a segment's `;parameters`; let `..`
    /// count an empty segment or merge empty segments first; read `\` as a separator or as part
    /// of a name; let a decoded NUL end the path or stay in a name; after splitting, resolve an
    /// escaped `.` or `..` or leave it a name; decode one round or as many as a chain of
    /// decoding hops might; recognise `.` and `..` only as written or also with control
    /// characters and trailing spaces around them; drop a name's trailing dots only or its
    /// trailing dots and spaces in any mix. A choice whose character the text does not
    /// hold changes nothing and is left out, so a plain path has one reading.
    ///
    /// ```
    /// use puddle_types::CanonicalPath;
    /// let read = |t: &str| -> Vec<Vec<String>> {
    ///     CanonicalPath::readings(t).iter().map(|p| p.segments().to_vec()).collect()
    /// };
    /// assert_eq!(read("/Admin/?q=1"), [["admin"]]);
    /// assert_eq!(read("/repos/a%2fb"), [vec!["repos", "a", "b"], vec!["repos", "a/b"]]);
    /// assert!(read("/admin%3fx").contains(&vec!["admin".to_owned()]));
    /// assert!(read("//evil.example/admin").contains(&vec!["admin".to_owned()]));
    /// ```
    #[must_use]
    pub fn readings(target: &str) -> Vec<Self> {
        let path = target_path(target);
        let decoded = percent_decoded(path);
        let mut texts: Vec<String> = vec![path.to_owned()];
        for decoded in [decode_once(path), decoded.clone()] {
            if let Some(end) = decoded.find(['?', '#']) {
                texts.push(decoded.get(..end).unwrap_or_default().to_owned());
            }
        }
        if target.contains('#') {
            let literal = target
                .find('?')
                .map_or(target, |query| target.get(..query).unwrap_or_default())
                .replace('#', "%23");
            texts.push(target_path(&literal).to_owned());
        }
        if let Some(rest) = decoded
            .strip_prefix(['/', '\\'])
            .and_then(|rest| rest.strip_prefix(['/', '\\']))
        {
            // More separators after the first two are skipped before the authority.
            let rest = rest.trim_start_matches(['/', '\\']);
            let after = rest
                .find(['/', '\\'])
                .map_or("", |at| rest.get(at..).unwrap_or_default());
            texts.push(after.to_owned());
        }
        let mut readings: Vec<Self> = Vec::new();
        for text in &texts {
            for read in Read::all_for(text) {
                let reading = Self {
                    segments: segments_read(text, read),
                };
                if !readings.contains(&reading) {
                    readings.push(reading);
                }
            }
        }
        readings
    }
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// One round of percent-decoding, including the `%uXXXX` form some servers accept. An escape
/// that is not valid stays as written.
fn decode_once(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        if byte == b'%' {
            if let (Some(b'u' | b'U'), Some(digits)) =
                (bytes.get(at + 1), bytes.get(at + 2..at + 6))
                && let Some(code) = digits
                    .iter()
                    .try_fold(0u32, |acc, &d| hex(d).map(|v| acc * 16 + u32::from(v)))
                && let Some(c) = char::from_u32(code)
            {
                let mut buffer = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                at += 6;
                continue;
            }
            if let (Some(&high), Some(&low)) = (bytes.get(at + 1), bytes.get(at + 2))
                && let (Some(high), Some(low)) = (hex(high), hex(low))
            {
                out.push(high * 16 + low);
                at += 3;
                continue;
            }
        }
        out.push(byte);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `text` with its percent-escapes decoded, as often as a server might decode them (a few
/// rounds), and nothing else changed. For values a rule reads beside the path, such as a
/// method named in a query string.
///
/// ```
/// assert_eq!(puddle_types::percent_decoded("%44ELETE"), "DELETE");
/// assert_eq!(puddle_types::percent_decoded("%2544ELETE"), "DELETE");
/// ```
#[must_use]
pub fn percent_decoded(text: &str) -> String {
    let mut text = text.to_owned();
    for _ in 0..DECODE_ROUNDS {
        let next = decode_once(&text);
        if next == text {
            break;
        }
        text = next;
    }
    text
}

/// `text` up to its first NUL.
fn to_nul(mut text: String) -> String {
    if let Some(end) = text.find('\0') {
        text.truncate(end);
    }
    text
}

fn canonical_segments(path: &str) -> Vec<String> {
    segments_read(path, Read::default())
}

/// One way servers read a path: a set of the flags below. Every combination is a reading,
/// because servers combine these choices in every way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Read(u16);

impl Read {
    /// Split where the path has a `/` or `\` as written and decode each segment on its own (a
    /// router that matches undecoded segments, for which `a%2fb` is one name); otherwise the
    /// whole path is decoded first.
    const SPLIT_FIRST: u16 = 1;
    /// Keep a segment's `;parameters` as part of it; servlet containers drop them.
    const KEEP_PARAMS: u16 = 2;
    /// Count an empty segment when `..` takes one away, as the WHATWG URL parser does
    /// (`/admin//..` is `/admin/`); otherwise empty segments go first, as when a server merges
    /// slashes.
    const KEEP_EMPTY: u16 = 4;
    /// Read `\` as part of a name, as nginx does (`/admin/a\b/..` is `/admin/` there);
    /// otherwise it separates segments, as on Windows and in the WHATWG URL parser.
    const BACKSLASH_IN_NAME: u16 = 8;
    /// Read a decoded NUL as part of a name; otherwise it ends the path, as C servers end it.
    const NUL_IN_NAME: u16 = 16;
    /// With [`Self::SPLIT_FIRST`]: resolve an escaped `.` or `..` (`%2e%2e`) as the WHATWG URL
    /// parser does; otherwise it is a name.
    const ESCAPED_DOTS: u16 = 32;
    /// Decode one round only, as nearly every server does; otherwise as many as a server behind
    /// another decoding hop might ([`percent_decoded`]).
    const ONE_ROUND: u16 = 64;
    /// Recognise `.` and `..` only as written; otherwise also with control characters and
    /// trailing spaces around them (`.. ` is `..` on Windows).
    const STRICT_DOTS: u16 = 128;
    /// Drop only the trailing dots of a name and keep a space before them, as servers that are
    /// not Windows do (`/x/ ./y` has three segments there); otherwise trailing dots and spaces go
    /// in any mix, as on Windows (`admin .` is `admin`).
    const DOTS_ONLY_TRIM: u16 = 256;
    /// Every flag above.
    const ALL: u16 = 511;

    /// Every reading that can differ from the others for `path`: a flag whose character the
    /// path does not hold changes nothing, so it is left out.
    fn all_for(path: &str) -> impl Iterator<Item = Self> {
        let decoded = percent_decoded(path);
        let mut relevant = 0;
        if path.contains('%') {
            relevant |= Self::SPLIT_FIRST | Self::ESCAPED_DOTS;
        }
        if decode_once(path).contains('%') {
            relevant |= Self::ONE_ROUND;
        }
        if decoded.contains(';') {
            relevant |= Self::KEEP_PARAMS;
        }
        if decoded.contains("..") {
            relevant |= Self::KEEP_EMPTY;
        }
        if decoded.contains('\\') {
            relevant |= Self::BACKSLASH_IN_NAME;
        }
        if decoded.contains('\0') {
            relevant |= Self::NUL_IN_NAME;
        }
        if decoded.contains(|c: char| c.is_control() || c == ' ') {
            relevant |= Self::STRICT_DOTS;
        }
        if decoded.contains(' ') && decoded.contains('.') {
            relevant |= Self::DOTS_ONLY_TRIM;
        }
        (0..=Self::ALL)
            .filter(move |flags| flags & !relevant == 0)
            .map(Self)
    }

    fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    fn decode(self, text: &str) -> String {
        if self.has(Self::ONE_ROUND) {
            decode_once(text)
        } else {
            percent_decoded(text)
        }
    }

    fn separators(self) -> &'static [char] {
        if self.has(Self::BACKSLASH_IN_NAME) {
            &['/']
        } else {
            &['/', '\\']
        }
    }
}

/// The segments of `path` in canonical form, read the way `read` says.
fn segments_read(path: &str, read: Read) -> Vec<String> {
    let mut segments: Vec<Option<String>> = Vec::new();
    if read.has(Read::SPLIT_FIRST) {
        for raw in path.split(read.separators()) {
            match raw {
                "." => {}
                ".." => {
                    segments.pop();
                }
                // A router that does not decode before matching never resolves an escaped dot.
                _ => push_segment(
                    &mut segments,
                    &read.decode(raw),
                    read,
                    read.has(Read::ESCAPED_DOTS),
                ),
            }
        }
    } else {
        let mut text = read.decode(path);
        if !read.has(Read::NUL_IN_NAME) {
            text = to_nul(text);
        }
        for raw in text.split(read.separators()) {
            push_segment(&mut segments, raw, read, true);
        }
    }
    segments.into_iter().flatten().collect()
}

/// Adds the decoded segment `raw` to `segments` in canonical form: with `dots`, `.` goes and
/// `..` takes the last one away; an empty one is kept (as `None`) only when `read` counts it.
fn push_segment(segments: &mut Vec<Option<String>>, raw: &str, read: Read, dots: bool) {
    let raw = if read.has(Read::KEEP_PARAMS) {
        raw
    } else {
        raw.split(';').next().unwrap_or_default()
    };
    let segment: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .to_lowercase();
    let spaced = segment.trim_end_matches(' ');
    let dots = dots && (!read.has(Read::STRICT_DOTS) || matches!(raw, "." | ".."));
    match spaced {
        "." if dots => return,
        ".." if dots => {
            segments.pop();
            return;
        }
        _ => {}
    }
    let name = if read.has(Read::DOTS_ONLY_TRIM) {
        spaced.trim_end_matches('.')
    } else {
        spaced.trim_end_matches(['.', ' '])
    };
    if !name.is_empty() {
        segments.push(Some(name.to_owned()));
    } else if read.has(Read::KEEP_EMPTY) {
        segments.push(None);
    }
}

/// Why a path pattern was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PathPatternError {
    /// Empty, not starting with `/`, or longer than [`MAX_PATH_PATTERN`].
    #[error("a path pattern starts with `/` and is at most {MAX_PATH_PATTERN} bytes")]
    Shape,
    /// A query or fragment: rules never see them.
    #[error("a path pattern has no query string or fragment: rules never see them")]
    Query,
    /// A `*` inside a segment (`/a*`, `/*b`): a segment is a name, `*` or `**` as a whole.
    #[error("`*` stands for a whole segment (`/repos/*/issues`), not part of one")]
    PartialWildcard,
    /// `**` anywhere but the end.
    #[error("`**` may only end a pattern (`/gists/**`)")]
    OpenNotLast,
    /// More than [`MAX_PATH_SEGMENTS`] segments.
    #[error("a path pattern has at most {MAX_PATH_SEGMENTS} segments")]
    TooLong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Name(String),
    One,
}

/// Which request paths a rule names: `/gists` is that path alone (and `/gists/`, and every
/// spelling of it), `/repos/*/issues` has any one segment in the middle, and a closing `/**`
/// takes any number of further segments, none included (`/gists/**` is `/gists` and everything
/// below it, `/**` every path).
///
/// The pattern is written in the same canonical form paths are compared in, so `/Gists/`,
/// `/%67ists` and `//gists` are the one pattern.
///
/// ```
/// use puddle_types::{CanonicalPath, PathPattern};
/// let pattern = PathPattern::parse("/Repos/*/issues/**").unwrap();
/// assert_eq!(pattern.to_string(), "/repos/*/issues/**");
/// assert!(pattern.matches(&CanonicalPath::new("/repos/me/issues")));
/// assert!(pattern.matches(&CanonicalPath::new("/repos/me/issues/12/comments")));
/// assert!(!pattern.matches(&CanonicalPath::new("/repos/me")));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPattern {
    segments: Vec<Segment>,
    open: bool,
}

/// Stands in for `*` while the text is canonicalised; no real segment can hold it, because
/// control characters are removed from them.
const ONE: &str = "\u{1}one";
const OPEN: &str = "\u{1}open";

impl PathPattern {
    /// Every path (`/**`).
    #[must_use]
    pub fn any() -> Self {
        Self {
            segments: Vec::new(),
            open: true,
        }
    }

    /// Parses `text`.
    ///
    /// # Errors
    /// [`PathPatternError`] for a pattern that is not a path or uses a wildcard in a way that
    /// is not supported.
    pub fn parse(text: &str) -> Result<Self, PathPatternError> {
        // The private-use characters are the wildcard stand-ins below, so text with them is not
        // a pattern someone wrote.
        if !text.starts_with('/')
            || text.len() > MAX_PATH_PATTERN
            || text
                .chars()
                .any(|c| c.is_control() || ('\u{e000}'..='\u{e001}').contains(&c))
        {
            return Err(PathPatternError::Shape);
        }
        if text.contains(['?', '#']) {
            return Err(PathPatternError::Query);
        }
        // Whole-segment wildcards are marked before decoding, so `%2a` stays a plain `*`.
        let marked: Vec<&str> = text
            .split('/')
            .map(|segment| match segment {
                "*" => ONE,
                "**" => OPEN,
                other => other,
            })
            .collect();
        let wildcards = marked.iter().filter(|s| matches!(**s, ONE | OPEN)).count();
        let canonical = canonical_segments_marked(&marked.join("/"));
        // An escape that decoded into a stand-in, or a `..` that took a wildcard away, would
        // change what the pattern says.
        if canonical
            .iter()
            .filter(|s| matches!(s.as_str(), ONE | OPEN))
            .count()
            != wildcards
        {
            return Err(PathPatternError::Shape);
        }
        let mut segments = Vec::new();
        let mut open = false;
        for (at, segment) in canonical.iter().enumerate() {
            match segment.as_str() {
                OPEN => {
                    open = true;
                    if at + 1 != canonical.len() {
                        return Err(PathPatternError::OpenNotLast);
                    }
                }
                ONE => segments.push(Segment::One),
                name if name.contains('*') => return Err(PathPatternError::PartialWildcard),
                name => segments.push(Segment::Name(name.to_owned())),
            }
        }
        if segments.len() > MAX_PATH_SEGMENTS {
            return Err(PathPatternError::TooLong);
        }
        Ok(Self { segments, open })
    }

    /// Whether `path` is one this pattern names.
    #[must_use]
    pub fn matches(&self, path: &CanonicalPath) -> bool {
        let have = path.segments();
        let fits = if self.open {
            have.len() >= self.segments.len()
        } else {
            have.len() == self.segments.len()
        };
        fits && self
            .segments
            .iter()
            .zip(have)
            .all(|(want, got)| match want {
                Segment::One => true,
                Segment::Name(name) => name == got,
            })
    }
}

/// [`canonical_segments`], keeping the two wildcard markers as they are: their control
/// character would otherwise be filtered out.
fn canonical_segments_marked(text: &str) -> Vec<String> {
    // The markers hold a control character that `canonical_segments` removes, so they are
    // swapped for private-use characters on the way in and back on the way out.
    const ONE_STAND: &str = "\u{e000}";
    const OPEN_STAND: &str = "\u{e001}";
    let swapped = text.replace(ONE, ONE_STAND).replace(OPEN, OPEN_STAND);
    canonical_segments(&swapped)
        .into_iter()
        .map(|segment| match segment.as_str() {
            ONE_STAND => ONE.to_owned(),
            OPEN_STAND => OPEN.to_owned(),
            _ => segment,
        })
        .collect()
}

impl fmt::Display for PathPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.segments.is_empty() && !self.open {
            return f.write_str("/");
        }
        let mut out = String::new();
        for segment in &self.segments {
            match segment {
                Segment::One => out.push_str("/*"),
                Segment::Name(name) => {
                    out.push('/');
                    out.push_str(name);
                }
            }
        }
        if self.open {
            out.push_str("/**");
        }
        f.write_str(&out)
    }
}

/// Why a method was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not an HTTP method")]
pub struct MethodError(String);

/// The methods a rule names; none named is every method.
///
/// ```
/// use puddle_types::Methods;
/// let methods = Methods::parse(["delete", "Put"]).unwrap();
/// assert_eq!(methods.to_string(), "DELETE,PUT");
/// assert!(methods.matches_any(["put"]));
/// assert!(Methods::any().matches_any(["PATCH"]));
/// assert!(Methods::parse(["GE T"]).is_err());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Methods(Vec<String>);

/// Most methods one rule names.
pub const MAX_METHODS: usize = 16;

impl Methods {
    /// Every method.
    #[must_use]
    pub fn any() -> Self {
        Self::default()
    }

    /// The methods named, upper-cased and without duplicates.
    ///
    /// # Errors
    /// [`MethodError`] for something that is not an HTTP token, or more than [`MAX_METHODS`].
    pub fn parse<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<Self, MethodError> {
        let mut methods: Vec<String> = Vec::new();
        for name in names {
            let valid = !name.is_empty()
                && name.len() <= 32
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
            if !valid {
                return Err(MethodError(name.to_owned()));
            }
            let upper = name.to_ascii_uppercase();
            if !methods.contains(&upper) {
                methods.push(upper);
            }
        }
        if methods.len() > MAX_METHODS {
            return Err(MethodError(format!("more than {MAX_METHODS} methods")));
        }
        methods.sort();
        Ok(Self(methods))
    }

    /// The methods, upper case and sorted; empty for every method.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.0
    }

    /// Whether any of `candidates` is named (case does not matter), or no method is named.
    #[must_use]
    pub fn matches_any<'a>(&self, candidates: impl IntoIterator<Item = &'a str>) -> bool {
        if self.0.is_empty() {
            return true;
        }
        candidates
            .into_iter()
            .any(|candidate| self.0.iter().any(|m| m.eq_ignore_ascii_case(candidate)))
    }
}

impl fmt::Display for Methods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join(","))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(path: &str) -> String {
        CanonicalPath::new(path).segments().join("/")
    }

    #[test]
    fn every_spelling_of_one_path_is_one_path() {
        for spelling in [
            "/admin",
            "/ADMIN",
            "/Admin/",
            "//admin",
            "///admin//",
            "/./admin",
            "/x/../admin",
            "/x/y/../../admin",
            "/%61dmin",
            "/%41DMIN",
            "/a%64min",
            "/%2fadmin",
            "/%2f%2fadmin",
            "/x/%2e%2e/admin",
            "/x/%2E%2E/admin",
            "/x/.%2e/admin",
            "/x/%2e./admin",
            "/x%2f..%2fadmin",
            "/x%5c..%5cadmin",
            "/x\\..\\admin",
            "/%252e%252e/admin",
            "/x/%252e%252e/admin",
            "/%25%32%65%25%32%65/admin",
            "/admin;x=1",
            "/admin;x=1/",
            "/;x/admin",
            "/admin.",
            "/admin..",
            "/admin%20",
            "/admin%2e",
            "/admin%00",
            "/admin%00.png",
            "/admin%u0000x",
            "/%u0061dmin",
            "/adm%0ain",
            "/adm\tin",
            "/../admin",
            "/../../../admin",
            "/..%2f..%2fadmin",
            "/./././admin",
            "/x/.. /admin",
            "/x/../ /admin",
            "/admin%20.",
            "/admin.%20.%20",
        ] {
            assert_eq!(canon(spelling), "admin", "{spelling:?}");
        }
    }

    #[test]
    fn different_paths_stay_different() {
        assert_eq!(canon("/"), "");
        assert_eq!(canon(""), "");
        assert_eq!(canon("/administrator"), "administrator");
        assert_eq!(canon("/a/b"), "a/b");
        assert_eq!(canon("/a/.b"), "a/.b");
        assert_eq!(canon("/v1.0/x"), "v1.0/x");
        assert_eq!(canon("/%zzadmin"), "%zzadmin");
        assert_eq!(canon("/%"), "%");
        assert_eq!(canon("/%4"), "%4");
        assert_eq!(canon("/%u12"), "%u12");
    }

    #[test]
    fn a_path_that_never_stops_decoding_ends_in_bounded_work() {
        let deep = "%25".repeat(200);
        assert_ne!(canon(&format!("/{deep}41")), "");
        assert!(canon("/%2525252525252525252e%2525252525252525252e/x").contains('%'));
    }

    #[test]
    fn a_path_of_the_wrong_bytes_is_still_a_path() {
        assert_eq!(canon("/%ff%fe/x"), "\u{fffd}\u{fffd}/x");
        assert_eq!(canon("/%c3%28"), "\u{fffd}(");
        assert_eq!(canon("/\u{e9}"), "\u{e9}");
        assert_eq!(canon("/%C3%89"), "\u{e9}");
    }

    #[test]
    fn a_pattern_is_kept_in_canonical_form() {
        for (text, kept) in [
            ("/", "/"),
            ("/**", "/**"),
            ("/gists", "/gists"),
            ("/Gists/", "/gists"),
            ("//gists//", "/gists"),
            ("/%67ists", "/gists"),
            ("/a/../gists", "/gists"),
            ("/repos/*/issues", "/repos/*/issues"),
            ("/repos/*/issues/**", "/repos/*/issues/**"),
        ] {
            assert_eq!(
                PathPattern::parse(text).unwrap().to_string(),
                kept,
                "{text}"
            );
        }
        // an encoded star is not a wildcard, and a segment cannot be one
        assert_eq!(
            PathPattern::parse("/repos/%2A/x"),
            Err(PathPatternError::PartialWildcard)
        );
    }

    #[test]
    fn a_pattern_that_is_not_a_path_is_refused() {
        assert_eq!(PathPattern::parse(""), Err(PathPatternError::Shape));
        assert_eq!(PathPattern::parse("gists"), Err(PathPatternError::Shape));
        assert_eq!(
            PathPattern::parse(&format!("/{}", "a".repeat(MAX_PATH_PATTERN))),
            Err(PathPatternError::Shape)
        );
        assert_eq!(PathPattern::parse("/a?b"), Err(PathPatternError::Query));
        assert_eq!(PathPattern::parse("/a#b"), Err(PathPatternError::Query));
        assert_eq!(
            PathPattern::parse("/a*"),
            Err(PathPatternError::PartialWildcard)
        );
        assert_eq!(
            PathPattern::parse("/*a"),
            Err(PathPatternError::PartialWildcard)
        );
        assert_eq!(
            PathPattern::parse("/***"),
            Err(PathPatternError::PartialWildcard)
        );
        assert_eq!(
            PathPattern::parse("/**/a"),
            Err(PathPatternError::OpenNotLast)
        );
        assert_eq!(
            PathPattern::parse("/**/**"),
            Err(PathPatternError::OpenNotLast)
        );
        let long = "/a".repeat(MAX_PATH_SEGMENTS + 1);
        assert_eq!(PathPattern::parse(&long), Err(PathPatternError::TooLong));
        assert!(PathPattern::parse(&"/a".repeat(MAX_PATH_SEGMENTS)).is_ok());
        // stand-in characters, control characters, and wildcards that decoding would make or
        // remove are refused
        for bad in [
            "/\u{e000}",
            "/\u{e001}",
            "/a\u{1}",
            "/\n",
            "/*/..",
            "/%u0001one",
        ] {
            let parsed = PathPattern::parse(bad);
            assert!(
                parsed.is_err() || !parsed.unwrap().matches(&CanonicalPath::new("/x")),
                "{bad:?}"
            );
        }
        assert_eq!(
            PathPattern::parse("/\u{e000}"),
            Err(PathPatternError::Shape)
        );
        assert_eq!(PathPattern::parse("/*/.."), Err(PathPatternError::Shape));
    }

    fn pattern_matches(pattern: &str, path: &str) -> bool {
        PathPattern::parse(pattern)
            .unwrap()
            .matches(&CanonicalPath::new(path))
    }

    #[test]
    fn a_pattern_without_wildcards_names_that_path_alone() {
        assert!(pattern_matches("/gists", "/gists"));
        assert!(pattern_matches("/gists", "/gists/"));
        assert!(pattern_matches("/gists", "//GISTS"));
        assert!(!pattern_matches("/gists", "/gists/1"));
        assert!(!pattern_matches("/gists", "/"));
        assert!(!pattern_matches("/gists", "/gist"));
        assert!(pattern_matches("/", "/"));
        assert!(pattern_matches("/", "//"));
        assert!(pattern_matches("/", "/x/.."));
        assert!(!pattern_matches("/", "/x"));
    }

    #[test]
    fn a_wildcard_takes_one_segment_and_a_closing_one_takes_the_rest() {
        assert!(pattern_matches("/repos/*/issues", "/repos/me/issues"));
        assert!(!pattern_matches("/repos/*/issues", "/repos/issues"));
        assert!(!pattern_matches("/repos/*/issues", "/repos/a/b/issues"));
        assert!(!pattern_matches("/repos/*/issues", "/repos/me/issues/1"));
        assert!(pattern_matches("/gists/**", "/gists"));
        assert!(pattern_matches("/gists/**", "/gists/1/comments"));
        assert!(!pattern_matches("/gists/**", "/gist"));
        assert!(pattern_matches("/**", "/"));
        assert!(pattern_matches("/**", "/a/b/c"));
        assert!(PathPattern::any().matches(&CanonicalPath::new("/anything")));
        // a segment that was spelled to hide the dots still meets the pattern
        assert!(pattern_matches("/gists/**", "/x/%2e%2e/gists/1"));
        assert!(pattern_matches(
            "/repos/*/issues",
            "/repos/x%2f..%2fme/issues"
        ));
    }

    #[test]
    fn methods_are_tokens_kept_upper_case_and_matched_without_case() {
        let methods = Methods::parse(["post", "POST", "Delete"]).unwrap();
        assert_eq!(methods.names(), ["DELETE", "POST"]);
        assert!(methods.matches_any(["Post"]));
        assert!(methods.matches_any(["GET", "delete"]));
        assert!(!methods.matches_any(["GET"]));
        assert!(!methods.matches_any([]));
        assert!(Methods::any().matches_any([]));
        for bad in ["", "GE T", "GET\n", "G,T", "a/b", &"X".repeat(33)] {
            assert!(Methods::parse([bad]).is_err(), "{bad:?}");
        }
        let many: Vec<String> = (0..=MAX_METHODS).map(|i| format!("M{i}")).collect();
        assert!(Methods::parse(many.iter().map(String::as_str)).is_err());
        assert_eq!(
            MethodError("x".into()).to_string(),
            "\"x\" is not an HTTP method"
        );
    }

    #[test]
    fn the_target_is_cut_to_its_path_without_mistaking_a_scheme() {
        assert_eq!(target_path("/a/b?x=1#y"), "/a/b");
        assert_eq!(
            target_path("/redirect/http://evil/admin"),
            "/redirect/http://evil/admin"
        );
        assert_eq!(target_path("http://u:p@h.example/x?q"), "/x");
        assert_eq!(target_path("*"), "*");
        assert_eq!(canon(target_path("*")), "*");
    }
}
