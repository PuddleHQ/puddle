// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashSet;

use super::*;

fn every_list() -> Vec<(&'static str, &'static [CatalogueEntry])> {
    let mut lists: Vec<_> = BUILT_IN_SETS.iter().map(|s| (s.slug, s.entries)).collect();
    lists.extend(SystemReason::ALL.map(|r| (r.as_str(), r.hosts())));
    lists
}

#[test]
fn r36_every_catalogue_pattern_passes_r04_and_none_repeats() {
    for (list, entries) in every_list() {
        assert!(!entries.is_empty(), "{list} is empty");
        let mut seen = HashSet::new();
        for entry in entries {
            let pattern = Pattern::parse(entry.pattern)
                .unwrap_or_else(|e| panic!("{list}: {}: {e}", entry.pattern));
            assert!(
                seen.insert(pattern.to_string()),
                "{list}: {} twice",
                entry.pattern
            );
            assert_ne!(entry.note, "");
        }
        assert_eq!(patterns(entries).len(), entries.len());
    }
}

#[test]
fn a_bad_catalogue_entry_is_skipped_not_fatal() {
    static BAD: [CatalogueEntry; 2] = [
        CatalogueEntry {
            pattern: "*.com",
            note: "a public suffix",
        },
        CatalogueEntry {
            pattern: "ok.example",
            note: "fine",
        },
    ];
    let kept: Vec<String> = patterns(&BAD).iter().map(ToString::to_string).collect();
    assert_eq!(kept, ["ok.example"]);
}

#[test]
fn r01_built_in_sets_ship_off_with_unique_slugs() {
    let mut slugs = HashSet::new();
    for set in BUILT_IN_SETS {
        assert!(!set.default_on, "{} ships on", set.slug);
        assert!(slugs.insert(set.slug));
        assert!(
            set.slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{}",
            set.slug
        );
        assert!(!set.name.is_empty() && !set.description.is_empty());
        assert_eq!(built_in(set.slug), Some(set));
    }
    assert_eq!(built_in("nope"), None);
}

#[test]
fn r41_system_reasons_round_trip_and_explain_themselves() {
    for reason in SystemReason::ALL {
        assert_eq!(SystemReason::parse(reason.as_str()), Some(reason));
        assert_eq!(reason.to_string(), reason.as_str());
        assert!(reason.describe().ends_with('.'));
    }
    assert_eq!(SystemReason::parse("vscode"), None);
    // Microsoft's hosts only for Microsoft's server; Open VSX only for code-server.
    let has = |r: SystemReason, host: &str| r.hosts().iter().any(|e| e.pattern == host);
    assert!(has(
        SystemReason::MicrosoftServer,
        "update.code.visualstudio.com"
    ));
    assert!(has(SystemReason::DirectSsh, "marketplace.visualstudio.com"));
    assert!(!has(
        SystemReason::CodeServer,
        "marketplace.visualstudio.com"
    ));
    assert!(has(SystemReason::CodeServer, "open-vsx.org"));
    // Each editor's hosts belong to its own reason.
    assert!(has(
        SystemReason::DirectSshCursor,
        "cursor.blob.core.windows.net"
    ));
    assert!(!has(
        SystemReason::DirectSsh,
        "cursor.blob.core.windows.net"
    ));
    assert!(has(
        SystemReason::DirectSshJetBrains,
        "downloads.marketplace.jetbrains.com"
    ));
    assert!(has(
        SystemReason::DirectSshZed,
        "release-assets.githubusercontent.com"
    ));
    assert!(!has(SystemReason::DirectSshZed, "plugins.jetbrains.com"));
    assert!(!has(SystemReason::MicrosoftServer, "open-vsx.org"));
    // mise's two hosts, and nothing a tool itself reaches (those are the user's to allow).
    assert!(has(SystemReason::Mise, "mise.jdx.dev"));
    assert!(has(SystemReason::Mise, "mise-versions.jdx.dev"));
    assert!(!has(SystemReason::Mise, "github.com"));
    assert!(!has(SystemReason::Mise, "nodejs.org"));
}

/// The provenance rule (R-36): a deny set's description names where its list came from, the
/// list's licence and the date. Returns what is missing.
fn provenance_gaps(set: &BuiltInSet) -> Vec<&'static str> {
    let text = set.description;
    let dated = text.as_bytes().windows(10).any(|w| {
        w.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
    });
    let mut gaps = Vec::new();
    if !text.contains("https://") {
        gaps.push("source");
    }
    if !text.contains("licence") {
        gaps.push("licence");
    }
    if !dated {
        gaps.push("date");
    }
    gaps
}

const TRACKER_FIXTURE: BuiltInSet = BuiltInSet {
    slug: "fixture-trackers",
    name: "Fixture trackers",
    description: "Hosts from https://lists.example/trackers (MIT licence), taken 2026-11-04.",
    default_on: false,
    effect: Effect::Deny,
    fill: Fill::solid(Rgb::new(0, 0, 0)),
    entries: &[
        entry!("tracker.example", "tracker"),
        entry!("*.ads.example", "ads"),
    ],
};

/// The deny sets among `sets` that lack provenance, with what each lacks.
fn deny_sets_missing_provenance(sets: &[BuiltInSet]) -> Vec<(&'static str, Vec<&'static str>)> {
    sets.iter()
        .filter(|set| set.effect == Effect::Deny)
        .map(|set| (set.slug, provenance_gaps(set)))
        .filter(|(_, gaps)| !gaps.is_empty())
        .collect()
}

fn no_gaps() -> Vec<(&'static str, Vec<&'static str>)> {
    Vec::new()
}

#[test]
fn r36_every_deny_set_names_its_source_licence_and_date() {
    assert_eq!(deny_sets_missing_provenance(BUILT_IN_SETS), no_gaps());
    assert_eq!(deny_sets_missing_provenance(&[TRACKER_FIXTURE]), no_gaps());
    assert_eq!(provenance_gaps(&TRACKER_FIXTURE), Vec::<&str>::new());
    let bare = BuiltInSet {
        description: "Tracker hosts.",
        ..TRACKER_FIXTURE
    };
    assert_eq!(
        deny_sets_missing_provenance(&[bare]),
        [("fixture-trackers", vec!["source", "licence", "date"])]
    );
}

#[test]
fn every_built_in_set_has_its_own_colour() {
    let mut seen = HashSet::new();
    for set in BUILT_IN_SETS {
        assert!(seen.insert(set.fill), "{} repeats a colour", set.slug);
    }
}

#[test]
fn r36_a_built_in_sets_entries_all_carry_its_effect() {
    for effect in [Effect::Allow, Effect::Deny] {
        let set = BuiltInSet {
            effect,
            ..TRACKER_FIXTURE
        };
        let entries = set_entries(&[set]);
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.effect == effect));
        assert!(
            entries
                .iter()
                .all(|e| e.set == RuleSetId::BuiltIn("fixture-trackers"))
        );
    }
}
