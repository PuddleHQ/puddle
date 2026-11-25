// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const OLD_JARGON: [&str; 6] = [
    "push list",
    "pull list",
    "listed repos",
    "no restart",
    "list yet",
    "as with a repository",
];

fn every_text() -> Vec<String> {
    [GitAccess::Push, GitAccess::Pull]
        .into_iter()
        .flat_map(|a| {
            [
                not_allowed(a, "forgejo.lab.test/acme/unlisted", Some("unlisted")),
                not_allowed(a, "forgejo.lab.test/acme/has space", None),
            ]
        })
        .collect()
}

#[test]
fn a_push_refusal_names_the_repository_and_gives_one_action_first() {
    assert_eq!(
        not_allowed(
            GitAccess::Push,
            "forgejo.lab.test/acme/unlisted",
            Some("unlisted")
        ),
        "Blocked this push: forgejo.lab.test/acme/unlisted is not in this workspace's allowed repositories for push. Allow it in the Puddle notice (\"Allow push for unlisted\"), or open the workspace's Git tab > Repositories and tick Push for it. Then run git push again."
    );
}

#[test]
fn no_refusal_text_carries_the_old_jargon() {
    for text in every_text() {
        for word in OLD_JARGON {
            assert!(!text.contains(word), "{word:?} in {text}");
        }
        assert!(text.starts_with("Blocked this "), "{text}");
    }
}

#[test]
fn only_the_unholdable_name_mentions_the_switch() {
    let held = not_allowed(GitAccess::Pull, "h/o/r", Some("r"));
    assert!(!held.contains("Only pull"), "{held}");
    let unheld = not_allowed(GitAccess::Pull, "h/o/r", None);
    assert!(
        unheld.contains("\"Only pull from allowed repositories\""),
        "{unheld}"
    );
}
