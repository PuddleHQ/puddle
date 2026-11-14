// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rule sets that ship with Puddle, and the hosts Puddle allows because of the user's own
//! setup ("System managed"), as data (`docs/spec/rules.md` §7).
//!
//! A built-in set is single-kind (R-36): every entry allows or every entry denies, as
//! [`BuiltInSet::effect`] says. System managed only allows. Entries are checked by the tests below
//! (R-4, no duplicates) and loaded into memory; nothing is copied into the database, so an update
//! of Puddle updates them.

use std::fmt;

use puddle_types::{Fill, Rgb, RuleSetId};

use crate::pattern::Pattern;
use crate::rule::Effect;

/// One entry of a built-in set or of a System managed reason: a pattern as the user would type
/// it (`example.com`, `*.example.com`) and what it is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogueEntry {
    /// The pattern.
    pub pattern: &'static str,
    /// What the host is for, in a few words.
    pub note: &'static str,
}

/// `entry!("example.com", "what for")`: a catalogue entry (a macro, so the data has no code of
/// its own to cover).
macro_rules! entry {
    ($pattern:expr, $note:expr $(,)?) => {
        CatalogueEntry {
            pattern: $pattern,
            note: $note,
        }
    };
}

/// A rule set that ships with Puddle. Read-only; on or off like any set (R-37).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltInSet {
    /// Stable id, `[a-z0-9-]+`; the wire id is `builtin:<slug>`.
    pub slug: &'static str,
    /// Display name.
    pub name: &'static str,
    /// One sentence on what the set is for.
    pub description: &'static str,
    /// Whether the set is on where the user never switched it. Every built-in set ships off
    /// (R-1: a fresh install has decided nothing).
    pub default_on: bool,
    /// What every entry of the set does. A set never mixes the two (R-36); a deny set names its
    /// source list, the list's licence and the date it was taken in `description` (R-36).
    pub effect: Effect,
    /// The colour the set is shown in. A built-in set is read-only, so this is fixed; a set the
    /// user made stores its own.
    pub fill: Fill,
    /// The hosts it allows or denies.
    pub entries: &'static [CatalogueEntry],
}

/// The built-in sets, in display order.
pub const BUILT_IN_SETS: &[BuiltInSet] = &[
    BuiltInSet {
        slug: "package-registries",
        name: "Package registries",
        description: "The public registries of npm, Yarn, PyPI, crates.io, Go modules, Maven Central, NuGet and RubyGems.",
        default_on: false,
        effect: Effect::Allow,
        fill: Fill::solid(Rgb::new(0x2f, 0x80, 0xc9)),
        entries: &[
            entry!("registry.npmjs.org", "npm"),
            entry!("registry.yarnpkg.com", "Yarn"),
            entry!("pypi.org", "PyPI index"),
            entry!("files.pythonhosted.org", "PyPI downloads"),
            entry!("crates.io", "crates.io"),
            entry!("index.crates.io", "crates.io index"),
            entry!("static.crates.io", "crates.io downloads"),
            entry!("proxy.golang.org", "Go module proxy"),
            entry!("sum.golang.org", "Go checksum database"),
            entry!("repo.maven.apache.org", "Maven Central"),
            entry!("repo1.maven.org", "Maven Central"),
            entry!("api.nuget.org", "NuGet"),
            entry!("rubygems.org", "RubyGems"),
            entry!("index.rubygems.org", "RubyGems index"),
        ],
    },
    BuiltInSet {
        slug: "debian-ubuntu",
        name: "Debian and Ubuntu packages",
        description: "The Debian and Ubuntu package archives, for apt.",
        default_on: false,
        effect: Effect::Allow,
        fill: Fill::solid(Rgb::new(0xc0, 0x4f, 0x7a)),
        entries: &[
            entry!("deb.debian.org", "Debian archive"),
            entry!("security.debian.org", "Debian security updates"),
            entry!("archive.ubuntu.com", "Ubuntu archive"),
            entry!("*.archive.ubuntu.com", "Ubuntu country mirrors"),
            entry!("security.ubuntu.com", "Ubuntu security updates"),
            entry!("ports.ubuntu.com", "Ubuntu archive for other architectures"),
        ],
    },
    BuiltInSet {
        slug: "github",
        name: "GitHub",
        description: "github.com, its API, and the hosts that serve repository archives, raw files and release downloads.",
        default_on: false,
        effect: Effect::Allow,
        fill: Fill::solid(Rgb::new(0x7a, 0x5b, 0xc7)),
        entries: &[
            entry!("github.com", "GitHub"),
            entry!("api.github.com", "GitHub API"),
            entry!("codeload.github.com", "repository archives"),
            entry!("raw.githubusercontent.com", "raw files"),
            entry!("objects.githubusercontent.com", "downloads"),
            entry!("release-assets.githubusercontent.com", "release downloads"),
        ],
    },
    BuiltInSet {
        slug: MISE_TOOLS_SLUG,
        name: "mise tools",
        description: "The hosts the managed mise layer downloads from: mise itself, its tool metadata, and the downloads of Node.js, Python, Go and Java. The set is hidden while the managed tools layer is switched off.",
        default_on: false,
        effect: Effect::Allow,
        fill: Fill::solid(Rgb::new(0xd0, 0x7a, 0x2c)),
        entries: &[
            entry!("mise.jdx.dev", "mise downloads"),
            entry!("mise-versions.jdx.dev", "tool versions"),
            entry!("mise-java.jdx.dev", "Java versions"),
            entry!("github.com", "Python and Java downloads"),
            entry!(
                "release-assets.githubusercontent.com",
                "Python and Java download files"
            ),
            entry!("nodejs.org", "Node.js downloads"),
            entry!("dl.google.com", "Go downloads"),
            entry!("tuf-repo-cdn.sigstore.dev", "Python download attestations"),
        ],
    },
];

/// The slug of the built-in set the managed mise layer's hosts live in; hidden while the layer is
/// off ([`crate::SystemPlan::hidden_sets`]).
pub const MISE_TOOLS_SLUG: &str = "mise-tools";

/// The built-in set with this slug.
#[must_use]
pub fn built_in(slug: &str) -> Option<&'static BuiltInSet> {
    BUILT_IN_SETS.iter().find(|set| set.slug == slug)
}

/// Why Puddle allows a System managed host: a choice the user made (R-41).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum SystemReason {
    /// The browser editor runs Microsoft's VS Code server (a global choice the user consented
    /// to): Puddle downloads it from Microsoft, and its extensions come from the Marketplace.
    MicrosoftServer,
    /// The browser editor runs the bundled code-server: its extensions come from Open VSX.
    CodeServer,
    /// Direct SSH is on for the workspace: desktop VS Code's Remote-SSH installs Microsoft's
    /// server and its extensions in it.
    DirectSsh,
    /// Direct SSH is on for the workspace: Cursor's Remote SSH installs Cursor's server and its
    /// extensions in it.
    DirectSshCursor,
    /// Direct SSH is on for the workspace: `JetBrains` Gateway downloads the IDE backend there and
    /// the backend installs plugins.
    DirectSshJetBrains,
    /// Direct SSH is on for the workspace: Zed downloads its remote server there.
    DirectSshZed,
    /// Puddle manages tools with mise in the workspace: it downloads the pinned mise and mise
    /// reads its version lists there.
    Mise,
}

const MICROSOFT_HOSTS: &[CatalogueEntry] = &[
    entry!(
        "update.code.visualstudio.com",
        "VS Code server downloads and updates",
    ),
    entry!(
        "vscode.download.prss.microsoft.com",
        "VS Code server downloads",
    ),
    entry!("marketplace.visualstudio.com", "Visual Studio Marketplace"),
    entry!("*.gallery.vsassets.io", "Marketplace extension files"),
    entry!("*.gallerycdn.vsassets.io", "Marketplace extension files"),
];

const CURSOR_HOSTS: &[CatalogueEntry] = &[
    entry!("cursor.blob.core.windows.net", "Cursor server downloads"),
    entry!("marketplace.cursorapi.com", "Cursor extension marketplace"),
    entry!("marketplace.cursor-cdn.com", "Cursor extension files"),
];

const JETBRAINS_HOSTS: &[CatalogueEntry] = &[
    entry!("download.jetbrains.com", "IDE backend downloads"),
    entry!("download-cdn.jetbrains.com", "IDE backend download files"),
    entry!("download-cf.jetbrains.com", "IDE backend download files"),
    entry!("cache-redirector.jetbrains.com", "IDE runtime downloads"),
    entry!("data.services.jetbrains.com", "list of IDE builds"),
    entry!("plugins.jetbrains.com", "JetBrains Marketplace"),
    entry!("downloads.marketplace.jetbrains.com", "plugin files"),
];

const ZED_HOSTS: &[CatalogueEntry] = &[
    entry!("github.com", "Zed remote server downloads"),
    entry!(
        "release-assets.githubusercontent.com",
        "Zed remote server download files"
    ),
];

const MISE_HOSTS: &[CatalogueEntry] = &[
    entry!("mise.jdx.dev", "mise downloads"),
    entry!("mise-versions.jdx.dev", "mise version lists"),
];

const OPEN_VSX_HOSTS: &[CatalogueEntry] = &[
    entry!("open-vsx.org", "Open VSX extensions"),
    entry!("openvsx.eclipsecontent.org", "Open VSX extension files"),
];

impl SystemReason {
    /// Every reason.
    pub const ALL: [Self; 7] = [
        Self::MicrosoftServer,
        Self::CodeServer,
        Self::DirectSsh,
        Self::DirectSshCursor,
        Self::DirectSshJetBrains,
        Self::DirectSshZed,
        Self::Mise,
    ];

    /// The stored and wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MicrosoftServer => "microsoft_server",
            Self::CodeServer => "code_server",
            Self::DirectSsh => "direct_ssh",
            Self::DirectSshCursor => "direct_ssh_cursor",
            Self::DirectSshJetBrains => "direct_ssh_jetbrains",
            Self::DirectSshZed => "direct_ssh_zed",
            Self::Mise => "mise",
        }
    }

    /// The reason with this name.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|reason| reason.as_str() == text)
    }

    /// The reason in words, as the Rules screen shows it.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::MicrosoftServer => {
                "You chose Microsoft's VS Code server for the browser editor: Puddle downloads it from Microsoft, and its extensions come from the Visual Studio Marketplace."
            }
            Self::CodeServer => {
                "The browser editor runs the bundled code-server: its extensions come from Open VSX."
            }
            Self::DirectSsh => {
                "Direct SSH is on for this workspace: desktop VS Code installs Microsoft's VS Code server and its extensions there."
            }
            Self::DirectSshCursor => {
                "Direct SSH is on for this workspace: Cursor installs its server and its extensions there."
            }
            Self::DirectSshJetBrains => {
                "Direct SSH is on for this workspace: `JetBrains` Gateway downloads the IDE backend there, and the backend installs plugins."
            }
            Self::DirectSshZed => {
                "Direct SSH is on for this workspace: Zed downloads its remote server there."
            }
            Self::Mise => "mise manages tools in this workspace.",
        }
    }

    /// The hosts this reason allows.
    #[must_use]
    pub fn hosts(self) -> &'static [CatalogueEntry] {
        match self {
            Self::MicrosoftServer | Self::DirectSsh => MICROSOFT_HOSTS,
            Self::CodeServer => OPEN_VSX_HOSTS,
            Self::DirectSshCursor => CURSOR_HOSTS,
            Self::DirectSshJetBrains => JETBRAINS_HOSTS,
            Self::DirectSshZed => ZED_HOSTS,
            Self::Mise => MISE_HOSTS,
        }
    }
}

impl fmt::Display for SystemReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The engine entries of `sets` (R-36), each carrying its set's effect.
pub(crate) fn set_entries<'a>(
    sets: impl IntoIterator<Item = &'a BuiltInSet>,
) -> Vec<crate::engine::SetEntry> {
    sets.into_iter()
        .flat_map(|set| {
            patterns(set.entries)
                .into_iter()
                .map(|pattern| crate::engine::SetEntry {
                    set: RuleSetId::BuiltIn(set.slug),
                    pattern,
                    workspace: None,
                    effect: set.effect,
                })
        })
        .collect()
}

/// `entries` as patterns. The catalogue is checked by tests, so a pattern that doesn't parse is
/// a build-time bug; it is skipped (and logged) rather than taking the store down.
pub(crate) fn patterns(entries: &'static [CatalogueEntry]) -> Vec<Pattern> {
    entries
        .iter()
        .filter_map(|entry| match Pattern::parse(entry.pattern) {
            Ok(pattern) => Some(pattern),
            Err(err) => {
                tracing::error!(pattern = entry.pattern, error = %err, "bad catalogue entry skipped");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
