// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decisions about the repository table that gates every push and pull.

use super::*;

// The repository table.

#[tokio::test]
async fn a_push_to_a_listed_repository_goes_out_with_the_credential() {
    let w = world();
    for (method, target) in [
        ("GET", PUSH_ADVERT),
        ("POST", "/acme/web.git/git-receive-pack"),
        ("POST", "/ACME/Web/git-receive-pack"),
    ] {
        let decision = w.decide("github.com", method, target, &[]).await;
        assert!(
            injected(&decision, &token_header("CANARY-WORK")),
            "{target}: {decision:?}"
        );
    }
    assert_eq!(w.events.events(), []);
}

#[tokio::test]
async fn a_push_to_a_repository_that_is_not_listed_is_refused_and_says_how_to_add_it() {
    let w = world();
    let decision = w
        .decide(
            "github.com",
            "GET",
            "/acme/other.git/info/refs?service=git-receive-pack",
            &[],
        )
        .await;
    let (status, code, message) = refusal(&decision);
    assert_eq!((status, code), (403, "push_denied"));
    assert_eq!(
        message,
        "Blocked this push: github.com/acme/other is not in this workspace's allowed repositories for push. Allow it in the Puddle notice (\"Allow push for other\"), or open the workspace's Git tab > Repositories and tick Push for it. Then run git push again."
    );
    assert_eq!(
        w.events.take(),
        [Event::GitAccessDenied {
            workspace: w.workspace.clone(),
            host: "github.com".into(),
            owner: "acme".into(),
            repo: "other".into(),
            access: GitAccess::Push,
        }]
    );
    assert_eq!(w.credentials.reads(), 0, "the credential was never read");
    // The refusal holds whoever supplies the credential: the workspace's own header does not
    // get a push through, and a Pull-only row does not either.
    w.list("github.com/acme/readonly", RepoToggles::PULL_ONLY);
    for target in [
        "/acme/other.git/git-receive-pack",
        "/acme/readonly.git/git-receive-pack",
    ] {
        let decision = w.decide("github.com", "POST", target, &[OWN]).await;
        assert_eq!(refusal(&decision).1, "push_denied", "{target}");
    }
}

#[tokio::test]
async fn the_same_refusal_is_one_notice_not_a_flood_and_a_later_one_is_a_new_notice() {
    tokio::time::pause();
    let w = world();
    let target = "/acme/other.git/git-receive-pack";
    for _ in 0..5 {
        let _ = w.decide("github.com", "POST", target, &[]).await;
    }
    assert_eq!(w.events.take().len(), 1);
    // Another repository is another notice.
    let _ = w
        .decide(
            "github.com",
            "POST",
            "/acme/third.git/git-receive-pack",
            &[],
        )
        .await;
    assert_eq!(w.events.take().len(), 1);
    tokio::time::advance(Duration::from_secs(11)).await;
    let _ = w.decide("github.com", "POST", target, &[]).await;
    assert_eq!(w.events.take().len(), 1);
}

#[tokio::test]
async fn turning_off_the_push_list_lets_any_push_through_and_a_name_the_table_cannot_hold_is_refused_with_the_switch()
 {
    let w = world();
    let target = "/acme/other.git/git-receive-pack";
    w.switches(OnlyListed::Neither);
    assert!(injected(
        &w.decide("github.com", "POST", target, &[]).await,
        &token_header("CANARY-WORK")
    ));
    // On again: a name the table has no spelling for can only be allowed by the switch.
    w.switches(OnlyListed::Push);
    let odd = w
        .decide("github.com", "POST", "/acme/a+b.git/git-receive-pack", &[])
        .await;
    let (status, code, message) = refusal(&odd);
    assert_eq!((status, code), (403, "push_denied"));
    assert!(
        message.contains("allowed repositories cannot hold"),
        "{message}"
    );
    assert!(
        message.contains("Only push to allowed repositories"),
        "{message}"
    );
    assert!(
        w.events.take().is_empty(),
        "there is no row to add, so no notice"
    );
}

#[tokio::test]
async fn an_azure_devops_project_with_a_space_can_be_listed_and_is_refused_with_the_add_it_notice_until_it_is()
 {
    let w = world();
    let target = "/org/My%20Project/_git/The%20Repo/git-receive-pack";
    let refused = w.decide("dev.azure.com", "POST", target, &[]).await;
    let (status, code, message) = refusal(&refused);
    assert_eq!((status, code), (403, "push_denied"));
    assert!(
        message.contains("dev.azure.com/org/my project/the repo"),
        "{message}"
    );
    assert!(
        message.contains("Allow push for my project/the repo"),
        "{message}"
    );
    assert_eq!(
        w.events.take(),
        [Event::GitAccessDenied {
            workspace: w.workspace.clone(),
            host: "dev.azure.com".into(),
            owner: "org".into(),
            repo: "my project/the repo".into(),
            access: GitAccess::Push,
        }]
    );
    // Listed with Push on, in the spelling the proxy reads: the same request is not refused.
    w.list("dev.azure.com/org/my project/the repo", RepoToggles::BOTH);
    for spelled in [
        target,
        "/org/my%20project/_git/The%20Repo.git/git-receive-pack",
        "/org/My%20project/_git/The%20repo/git-receive-pack",
    ] {
        let decision = w.decide("dev.azure.com", "POST", spelled, &[]).await;
        assert!(
            !matches!(decision, InjectDecision::Refuse(_)),
            "{spelled}: {decision:?}"
        );
    }
    // A `+` is a plus on a path, not a space: another name, not listed.
    let plus = w
        .decide(
            "dev.azure.com",
            "POST",
            "/org/My+Project/_git/The%20Repo/git-receive-pack",
            &[],
        )
        .await;
    assert_eq!(refusal(&plus).1, "push_denied");
}

#[tokio::test]
async fn a_repository_in_nested_groups_is_listed_and_credentialed_by_its_top_group() {
    let w = World::new();
    w.identity("Lab", "gitlab.example", &["group"], false, "CANARY-LAB");
    let push = "/Group/Sub/Proj.git/info/refs?service=git-receive-pack";
    let refused = w.decide("gitlab.example", "GET", push, &[]).await;
    let (status, code, message) = refusal(&refused);
    assert_eq!((status, code), (403, "push_denied"));
    assert!(
        message.contains("gitlab.example/group/sub/proj"),
        "{message}"
    );
    assert_eq!(
        w.events.take(),
        [Event::GitAccessDenied {
            workspace: w.workspace.clone(),
            host: "gitlab.example".into(),
            owner: "group".into(),
            repo: "sub/proj".into(),
            access: GitAccess::Push,
        }]
    );
    // A fetch is not held by the push list and gets the credential of the owner `group`.
    let fetch = w
        .decide(
            "gitlab.example",
            "POST",
            "/group/sub/proj.git/git-upload-pack",
            &[],
        )
        .await;
    assert!(injected(&fetch, &token_header("CANARY-LAB")), "{fetch:?}");
    // Listed, the push goes out; the owner's other repositories stay off the list.
    w.list("gitlab.example/group/sub/proj", RepoToggles::BOTH);
    let pushed = w.decide("gitlab.example", "GET", push, &[]).await;
    assert!(injected(&pushed, &token_header("CANARY-LAB")), "{pushed:?}");
    let other = w
        .decide(
            "gitlab.example",
            "POST",
            "/group/sub/other/git-receive-pack",
            &[],
        )
        .await;
    assert_eq!(refusal(&other).1, "push_denied");
    // The workspace's own header does not get it past the list.
    let own = w
        .decide(
            "gitlab.example",
            "POST",
            "/group/sub/deeper/other/git-receive-pack",
            &[OWN],
        )
        .await;
    assert_eq!(refusal(&own).1, "push_denied");
}

#[tokio::test]
async fn a_path_that_could_be_two_nested_repositories_or_is_too_deep_is_refused_not_guessed() {
    let w = World::new();
    w.identity("Lab", "gitlab.example", &["group"], false, "CANARY-LAB");
    w.switches(OnlyListed::Neither);
    for target in [
        "/group/proj/info/lfs/info/refs?service=git-upload-pack",
        "/group/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/git-upload-pack",
    ] {
        let decision = w.decide("gitlab.example", "POST", target, &[]).await;
        let (status, code, _) = refusal(&decision);
        assert_eq!((status, code), (400, "bad_git_path"), "{target}");
    }
    // A page of the host that happens to end in a Git word is not a Git request.
    for target in [
        "/group/proj/-/blob/main/info/refs",
        "/group/sub/proj/blob/main/info/refs",
    ] {
        let decision = w.decide("gitlab.example", "GET", target, &[]).await;
        assert_eq!(decision, InjectDecision::PassThrough, "{target}");
    }
}

#[tokio::test]
async fn a_fetch_from_anywhere_is_allowed_until_the_pull_list_is_turned_on() {
    let w = world();
    let anywhere = "/someone/private.git/info/refs?service=git-upload-pack";
    assert!(injected(
        &w.decide("github.com", "GET", anywhere, &[]).await,
        &token_header("CANARY-PERSONAL")
    ));
    w.switches(OnlyListed::Both);
    let decision = w.decide("github.com", "GET", anywhere, &[]).await;
    let (status, code, message) = refusal(&decision);
    assert_eq!((status, code), (403, "pull_denied"));
    assert_eq!(
        message,
        "Blocked this fetch: github.com/someone/private is not in this workspace's allowed repositories for pull. Allow it in the Puddle notice (\"Allow pull for private\"), or open the workspace's Git tab > Repositories and tick Pull for it. Then run the fetch or pull again."
    );
    assert!(matches!(
        w.events.take().as_slice(),
        [Event::GitAccessDenied { access: GitAccess::Pull, repo, .. }] if repo == "private"
    ));
    // A row with Pull off is refused too; the workspace's own header does not get past it.
    w.list("github.com/someone/pushonly", RepoToggles::PUSH_ONLY);
    let decision = w
        .decide(
            "github.com",
            "POST",
            "/someone/pushonly.git/git-upload-pack",
            &[OWN],
        )
        .await;
    assert_eq!(refusal(&decision).1, "pull_denied");
    // Listed with Pull on, it goes out.
    let listed = w.decide("github.com", "GET", FETCH, &[]).await;
    assert!(injected(&listed, &token_header("CANARY-WORK")));
}

// The switches decide a name the table cannot hold, for a pull as for a push.

#[tokio::test]
async fn a_name_the_table_cannot_hold_follows_the_switch_for_a_push_and_for_a_pull() {
    let w = World::new();
    w.identity("Personal", "github.com", &[], true, "CANARY-PERSONAL");
    let push = ("POST", "/acme/a+b.git/git-receive-pack");
    let pull = ("POST", "/acme/a+b.git/git-upload-pack");
    for only in [
        OnlyListed::Neither,
        OnlyListed::Push,
        OnlyListed::Pull,
        OnlyListed::Both,
    ] {
        let (only_push, only_pull) = (only.push(), only.pull());
        w.switches(only);
        for ((method, target), only_listed, code) in [
            (push, only_push, "push_denied"),
            (pull, only_pull, "pull_denied"),
        ] {
            let decision = w.decide("github.com", method, target, &[]).await;
            if only_listed {
                assert_eq!(
                    refusal(&decision).1,
                    code,
                    "{target} {only_push} {only_pull}"
                );
            } else {
                assert!(
                    injected(&decision, &token_header("CANARY-PERSONAL")),
                    "{target} {only_push} {only_pull}: {decision:?}"
                );
            }
        }
    }
}
