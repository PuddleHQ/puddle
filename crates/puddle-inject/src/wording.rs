// SPDX-License-Identifier: AGPL-3.0-or-later
//! The words of a Git refusal the workspace's allowed repositories cause.
//!
//! One term, "allowed repositories", is used here, on the workspace's Git tab, in the notice and in
//! the spec (CD-21). The proxy puts `puddle: ` in front. The text says what Puddle did, then the one action to take first, then what
//! to run again; the switch that turns the check off is named on the Git tab, not here.

use puddle_types::GitAccess;

/// The guest's message for a push or fetch to `what` (`host/owner/repo`) that the allowed
/// repositories do not cover. `name` is the repository's own name when the notice can offer to
/// allow it; `None` when the name cannot be held in the table.
pub(crate) fn not_allowed(access: GitAccess, what: &str, name: Option<&str>) -> String {
    let (blocked, tick, again, switch) = match access {
        GitAccess::Push => (
            "push",
            "Push",
            "git push",
            "Only push to allowed repositories",
        ),
        GitAccess::Pull => (
            "fetch",
            "Pull",
            "the fetch or pull",
            "Only pull from allowed repositories",
        ),
    };
    match name {
        Some(name) => format!(
            "Blocked this {blocked}: {what} is not in this workspace's allowed repositories for {access}. Allow it in the Puddle notice (\"Allow {access} for {name}\"), or open the workspace's Git tab > Repositories and tick {tick} for it. Then run {again} again.",
            access = access_word(access),
        ),
        None => format!(
            "Blocked this {blocked}: {what} has a name the workspace's allowed repositories cannot hold. Open the workspace's Git tab > Repositories and turn off \"{switch}\" (this lets the workspace {blocked} to any repository its credential can reach). Then run {again} again.",
        ),
    }
}

fn access_word(access: GitAccess) -> &'static str {
    match access {
        GitAccess::Push => "push",
        GitAccess::Pull => "pull",
    }
}

#[cfg(test)]
mod tests {
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
}
