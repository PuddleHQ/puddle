// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;
use std::sync::Arc;

use puddle_secrets::{AccountName, HostName, SourceSpec};
use puddle_store::{
    Author, Coverage, CredentialBinding, IdentityDraft, Limits, ManualClock, Owner, Store,
};
use puddle_types::WorkspaceId;

use super::*;

/// A binding for `host`. One on a port other than 443 can no longer be made (the store refuses
/// it), but an older database may still hold one, so it is read back from its stored form.
fn credential(host: &str, owners: &[&str], rest: bool) -> CredentialBinding {
    let plain = HostName::new(host.split(':').next().unwrap()).unwrap();
    let made = CredentialBinding::new(
        &plain,
        SourceSpec::Gh {
            host: plain.clone(),
            account: AccountName::new("me").unwrap(),
        },
        Coverage::new(
            owners
                .iter()
                .map(|o| Owner::new(o).unwrap())
                .collect::<BTreeSet<_>>(),
            rest,
        )
        .unwrap(),
    )
    .unwrap();
    let stored = serde_json::to_string(&made)
        .unwrap()
        .replace(plain.as_str(), host);
    serde_json::from_str(&stored).unwrap()
}

/// A workspace with these identities attached in this order, through a real store: what the
/// host reads.
fn git(identities: Vec<(&str, Vec<CredentialBinding>)>) -> WorkspaceGit {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let ws = WorkspaceId::new("alpha").unwrap();
    puddle_store::testing::own(&store, &ws);
    for (name, credentials) in identities {
        let identity = store
            .create_identity(
                puddle_store::UserId::OWNER,
                IdentityDraft {
                    label: name.to_owned(),
                    author: Author::new(name, &format!("{}@example.org", name.to_lowercase()))
                        .unwrap(),
                    credentials,
                },
            )
            .unwrap();
        store.attach_identity(&ws, identity.id, None).unwrap();
    }
    store.workspace_git(&ws).unwrap()
}

#[test]
fn the_ssh_names_are_the_built_in_hosts_and_the_credential_hosts_without_wildcards() {
    let names = |git: &WorkspaceGit| -> Vec<String> {
        ssh_names(git).iter().map(ToString::to_string).collect()
    };
    let none = names(&git(vec![]));
    assert_eq!(none.len(), 8, "{none:?}");
    assert!(none.contains(&"ssh.github.com".to_owned()));
    // A sorted list with no duplicates, whatever the order of the identities.
    let mut sorted = none.clone();
    sorted.sort();
    assert_eq!(none, sorted);

    let with = git(vec![
        ("Corp", vec![credential("Git.Corp.Example", &[], true)]),
        // Azure DevOps adds `*.visualstudio.com`, which is no name to list a key under.
        ("Azure", vec![credential("dev.azure.com", &[], true)]),
        // Another port is never decrypted, so its SSH is no Git name either.
        ("Odd", vec![credential("git.odd.example:8443", &[], true)]),
        // A built-in name is listed once.
        ("Hub", vec![credential("github.com", &[], true)]),
    ]);
    let listed = names(&with);
    assert_eq!(listed.len(), 10, "{listed:?}");
    assert!(listed.contains(&"git.corp.example".to_owned()));
    assert!(!listed.iter().any(|n| n.contains('*') || n.contains("odd")));

    let served = ssh_credential_hosts(&with);
    assert!(served.contains(&Host::parse_normalised("git.corp.example").unwrap()));
    assert!(served.contains(&Host::parse_normalised("dev.azure.com").unwrap()));
    assert!(!served.contains(&Host::parse_normalised("git.odd.example").unwrap()));
}

#[test]
fn a_listing_has_the_lines_of_its_keys_for_its_names() {
    let keys = Arc::new(GitSshHostKeys::generate().unwrap());
    let listing = SshListing::for_git(Arc::clone(&keys), &git(vec![]));
    let lines = listing.lines().unwrap();
    assert_eq!(lines.lines().count(), 8 * 2 * 3);
    assert_eq!(Some(lines), keys.known_hosts(&listing.hosts));
}

#[test]
fn a_credential_decrypts_its_host_and_azure_devops_its_legacy_names_too() {
    assert_eq!(decrypt_patterns("GitHub.com"), ["github.com"]);
    assert_eq!(decrypt_patterns("github.com:443"), ["github.com"]);
    assert_eq!(
        decrypt_patterns("dev.azure.com"),
        ["dev.azure.com", "*.visualstudio.com"]
    );
    // Only port 443 is terminated.
    assert_eq!(decrypt_patterns("ghe.example:8443"), [] as [String; 0]);
}

#[test]
fn the_decrypt_set_is_the_credentials_hosts_of_the_attached_identities() {
    let set = decrypt_set(&git(vec![
        ("Ada", vec![credential("github.com", &[], true)]),
        (
            "Bob",
            vec![
                credential("dev.azure.com", &["contoso"], false),
                // On another port, an address: never decrypted, and not a failure.
                credential("ghe.example:8443", &[], true),
                credential("10.0.0.5", &[], true),
            ],
        ),
    ]));
    let host = |h: &str| Host::parse_normalised(h).unwrap();
    for yes in ["github.com", "dev.azure.com", "contoso.visualstudio.com"] {
        assert!(set.contains(&host(yes)), "{yes}");
    }
    for no in ["api.github.com", "gitlab.com", "ghe.example", "10.0.0.5"] {
        assert!(!set.contains(&host(no)), "{no}");
    }
    assert!(decrypt_set(&git(vec![])).is_empty());
}

#[test]
fn globs_cover_the_url_forms_of_a_host_and_an_owner_in_any_case() {
    assert_eq!(
        remote_globs("github.com", Some("Acme")),
        [
            "https://[gG][iI][tT][hH][uU][bB].[cC][oO][mM]/[aA][cC][mM][eE]/**",
            "https://*@[gG][iI][tT][hH][uU][bB].[cC][oO][mM]/[aA][cC][mM][eE]/**",
        ]
    );
    assert_eq!(
        remote_globs("gitlab.com", None),
        [
            "https://[gG][iI][tT][lL][aA][bB].[cC][oO][mM]/**",
            "https://*@[gG][iI][tT][lL][aA][bB].[cC][oO][mM]/**",
        ]
    );
    let ado = remote_globs("dev.azure.com", Some("contoso"));
    assert_eq!(ado.len(), 4);
    assert!(
        ado[2].contains("[cC][oO][nN][tT][oO][sS][oO].[vV][iI]"),
        "{ado:?}"
    );
    let ado_rest = remote_globs("dev.azure.com", None);
    assert!(
        ado_rest[2].starts_with("https://*.[vV][iI][sS]"),
        "{ado_rest:?}"
    );
    assert!(
        ado_rest[3].starts_with("https://*@*.[vV][iI][sS]"),
        "{ado_rest:?}"
    );
}

#[test]
fn the_first_identity_is_the_fallback_and_a_named_owner_beats_the_rest_of_a_host() {
    // Personal is first and covers the rest of github.com; Work names acme.
    let a = authors(&git(vec![
        ("Personal", vec![credential("github.com", &[], true)]),
        ("Work", vec![credential("github.com", &["acme"], false)]),
    ]));
    assert_eq!(a.fallback.as_ref().unwrap().name(), "Personal");
    // The rest-of-host rule is written first, so the named owner's wins on acme's remotes.
    let names: Vec<&str> = a.rules.iter().map(|r| r.author().name()).collect();
    assert_eq!(names, ["Personal", "Work"]);
    assert!(a.rules[0].globs()[0].ends_with("/**") && !a.rules[0].globs()[0].contains("[aA]"));
    assert!(a.rules[1].globs()[0].contains("[aA][cC][mM][eE]"));
}

#[test]
fn among_identities_that_match_equally_the_first_in_the_list_is_written_last() {
    let a = authors(&git(vec![
        ("First", vec![credential("github.com", &["a"], false)]),
        ("Second", vec![credential("gitlab.com", &["a"], false)]),
    ]));
    let names: Vec<&str> = a.rules.iter().map(|r| r.author().name()).collect();
    assert_eq!(names, ["Second", "First"]);
}

#[test]
fn an_identity_without_credentials_is_only_a_fallback_and_nobody_is_none() {
    let a = authors(&git(vec![("Solo", vec![])]));
    assert_eq!(a.fallback.as_ref().unwrap().email(), "solo@example.org");
    assert_eq!(a.rules, []);
    assert_eq!(authors(&git(vec![])), Authors::default());
}

#[test]
fn an_author_git_cannot_keep_is_left_out_not_fatal() {
    let mut with_bad = git(vec![
        ("Good", vec![credential("github.com", &["a"], false)]),
        ("Bad", vec![credential("gitlab.com", &["a"], false)]),
    ]);
    with_bad.identities[1].author.name = "two\nlines".to_owned();
    let a = authors(&with_bad);
    let names: Vec<&str> = a.rules.iter().map(|r| r.author().name()).collect();
    assert_eq!(names, ["Good"]);
    // The user is told which identity gave nothing and why.
    assert_eq!(a.left_out.len(), 1);
    assert!(
        a.left_out[0].starts_with("the identity Bad gives commits no author:"),
        "{:?}",
        a.left_out
    );
    with_bad.identities[0].author.email = String::new();
    let none = authors(&with_bad);
    assert_eq!((none.fallback, none.rules), (None, vec![]));
    assert_eq!(none.left_out.len(), 2);
}

#[test]
fn what_was_left_out_is_one_problem_that_names_each_cause_and_none_when_nothing_was() {
    let ws = "Alpha";
    assert_eq!(left_out_problem(ws, &Authors::default()), None);
    let mut bad = git(vec![("Bad", vec![credential("github.com", &["a"], false)])]);
    bad.identities[0].author.name = "two\nlines".to_owned();
    let (title, detail) = left_out_problem(ws, &authors(&bad)).unwrap();
    assert_eq!(title, "Some of Alpha's commit authors are not applied");
    assert!(
        detail.starts_with("the identity Bad gives commits no author:")
            && detail.ends_with("gets the first identity's author."),
        "{detail}"
    );
}

#[test]
fn many_owners_are_split_into_rules_and_too_many_rules_drop_the_lowest_priority() {
    let owners: Vec<String> = (0..20).map(|i| format!("org{i}")).collect();
    let refs: Vec<&str> = owners.iter().map(String::as_str).collect();
    // 20 owners x 2 globs = 40 globs: two rules, neither over the limit.
    let a = authors(&git(vec![(
        "Many",
        vec![credential("github.com", &refs, false)],
    )]));
    assert_eq!(a.rules.len(), 2);
    assert!(
        a.rules
            .iter()
            .all(|r| r.globs().len() <= MAX_GIT_AUTHOR_GLOBS)
    );

    // A hundred identities on a hundred hosts: more rules than a plan holds.
    let names: Vec<String> = (0..100).map(|i| format!("Id{i}")).collect();
    let many: Vec<(&str, Vec<CredentialBinding>)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            (
                n.as_str(),
                vec![credential(&format!("host{i}.example"), &["x"], false)],
            )
        })
        .collect();
    let a = authors(&git(many));
    assert_eq!(a.rules.len(), MAX_GIT_AUTHOR_RULES);
    // The first identity (highest priority, written last) is kept.
    assert_eq!(a.rules.last().unwrap().author().name(), "Id0");
    // And the user is told how many went and who is affected.
    assert_eq!(a.left_out.len(), 1);
    assert!(
        a.left_out[0].contains(&format!(
            "{} of the author rules are left out",
            100 - MAX_GIT_AUTHOR_RULES
        )) && a.left_out[0].contains("at most"),
        "{:?}",
        a.left_out
    );
}

/// Real git, a system config that includes the one the boot plan writes, and repositories
/// with fake remotes: which author does a commit in each get?
mod with_real_git {
    use std::path::Path;
    use std::process::Command;

    use puddle_boot::BootPlan;
    use puddle_compute::ImageConfig;

    use super::*;

    /// `git` with none of the caller's `GIT_*` variables (a hook exports some), none of the
    /// user's own configuration, and `root`'s `etc/gitconfig` as the system config.
    fn git_in(root: &Path, dir: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("git");
        cmd.env_clear().envs(
            std::env::vars_os().filter(|(key, _)| !key.to_string_lossy().starts_with("GIT_")),
        );
        cmd.current_dir(dir)
            .env("GIT_CONFIG_SYSTEM", root.join("etc/gitconfig"))
            .env("GIT_CONFIG_GLOBAL", root.join("no-such-global"))
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("xdg"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .output()
            .unwrap()
    }

    /// Writes the plan's git files under `root` as the guest would have them, and a system
    /// config that includes Puddle's, the way `boot.sh` does.
    fn install(root: &Path, authors: &Authors) {
        let mut builder = BootPlan::builder(&ImageConfig::default());
        if let Some(fallback) = &authors.fallback {
            builder = builder.git_identity(fallback.clone());
        }
        for rule in &authors.rules {
            builder = builder.git_author_rule(rule.clone());
        }
        let plan = builder.build().unwrap();
        for file in plan.files() {
            let path = file.path().as_str();
            if path.starts_with("/etc/puddle/") {
                let target = root.join(path.trim_start_matches('/'));
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                std::fs::write(target, file.contents()).unwrap();
            }
        }
        let include = root.join("etc/puddle/gitconfig");
        std::fs::write(
            root.join("etc/gitconfig"),
            format!(
                "[include]\n\tpath = {}\n",
                include.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
    }

    /// The `user.email` a repository with these remotes gets.
    fn email(root: &Path, remotes: &[&str]) -> Option<String> {
        let repo = tempfile::tempdir().unwrap();
        assert!(git_in(root, repo.path(), &["init", "-q"]).status.success());
        for (i, url) in remotes.iter().enumerate() {
            let name = format!("r{i}");
            let out = git_in(root, repo.path(), &["remote", "add", &name, url]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{stderr}");
        }
        let out = git_in(root, repo.path(), &["config", "user.email"]);
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap().trim().to_owned())
    }

    fn three_identities() -> Authors {
        authors(&git(vec![
            ("Personal", vec![credential("github.com", &[], true)]),
            ("Work", vec![credential("github.com", &["acme"], false)]),
            (
                "Contoso",
                vec![credential("dev.azure.com", &["contoso"], false)],
            ),
        ]))
    }

    #[test]
    fn a_repository_gets_the_author_of_the_identity_that_covers_its_remote() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), &three_identities());
        let want = |remotes: &[&str], who: &str| {
            assert_eq!(
                email(root.path(), remotes).as_deref(),
                Some(format!("{who}@example.org").as_str()),
                "{remotes:?}"
            );
        };
        // No remote, a host nobody covers, an owner nobody names on a host with no rest rule
        // for it: the first identity's author.
        want(&[], "personal");
        want(&["https://gitlab.com/acme/web.git"], "personal");
        want(&["https://dev.azure.com/other/p/_git/r"], "personal");
        // The rest of github.com is Personal's; acme is named by Work, whatever the case or
        // the `user@` in the URL.
        want(&["https://github.com/someone/else.git"], "personal");
        for url in [
            "https://github.com/acme/web.git",
            "https://github.com/ACME/web.git",
            "https://GitHub.com/acme/web",
            "https://x-access-token@github.com/Acme/web.git",
            "https://user:secret@github.com/acme/web.git",
        ] {
            want(&[url], "work");
        }
        // Azure DevOps by either name, with and without the `user@` the web UI copies.
        for url in [
            "https://dev.azure.com/contoso/proj/_git/repo",
            "https://contoso@dev.azure.com/contoso/proj/_git/repo",
            "https://Contoso.VisualStudio.com/proj/_git/repo",
            "https://contoso.visualstudio.com/DefaultCollection/proj/_git/repo",
        ] {
            want(&[url], "contoso");
        }
        // Two remotes of two identities: the more specific wins, whatever the remote order.
        want(
            &[
                "https://github.com/someone/else.git",
                "https://github.com/acme/web.git",
            ],
            "work",
        );
        want(
            &[
                "https://github.com/acme/web.git",
                "https://github.com/someone/else.git",
            ],
            "work",
        );
    }

    #[test]
    fn the_first_of_two_equal_identities_wins_a_repository_with_both() {
        let root = tempfile::tempdir().unwrap();
        install(
            root.path(),
            &authors(&git(vec![
                ("First", vec![credential("github.com", &["a"], false)]),
                ("Second", vec![credential("gitlab.com", &["b"], false)]),
            ])),
        );
        let remotes = ["https://gitlab.com/b/x.git", "https://github.com/a/y.git"];
        assert_eq!(
            email(root.path(), &remotes).as_deref(),
            Some("first@example.org")
        );
        let flipped = [remotes[1], remotes[0]];
        assert_eq!(
            email(root.path(), &flipped).as_deref(),
            Some("first@example.org")
        );
    }

    #[test]
    fn a_workspace_with_no_identity_names_no_author_and_a_push_url_alone_decides_nothing() {
        let root = tempfile::tempdir().unwrap();
        install(root.path(), &authors(&git(vec![])));
        assert_eq!(
            email(root.path(), &["https://github.com/acme/web.git"]),
            None
        );
        // `pushurl` is not a remote url: a repository whose only match is a push url keeps the
        // fallback.
        install(root.path(), &three_identities());
        let repo = tempfile::tempdir().unwrap();
        assert!(
            git_in(root.path(), repo.path(), &["init", "-q"])
                .status
                .success()
        );
        git_in(
            root.path(),
            repo.path(),
            &["remote", "add", "o", "https://gitlab.com/x/y"],
        );
        git_in(
            root.path(),
            repo.path(),
            &[
                "config",
                "remote.o.pushurl",
                "https://github.com/acme/web.git",
            ],
        );
        let out = git_in(root.path(), repo.path(), &["config", "user.email"]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "personal@example.org"
        );
    }
}
