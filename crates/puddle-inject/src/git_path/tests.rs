// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn git_of(classified: Classified) -> Option<GitPath> {
    match classified {
        Classified::Git(found) => Some(found),
        Classified::NotGit | Classified::Ambiguous(_) => None,
    }
}

fn why(classified: &Classified) -> Option<&'static str> {
    match classified {
        Classified::Ambiguous(why) => Some(why),
        Classified::NotGit | Classified::Git(_) => None,
    }
}

fn git(host: &str, method: &str, path: &str, query: Option<&str>) -> GitPath {
    git_of(classify(host, method, path, query))
        .unwrap_or_else(|| panic!("{method} {path} on {host} is not a Git request"))
}

fn ambiguous(host: &str, method: &str, path: &str) -> &'static str {
    why(&classify(host, method, path, None))
        .unwrap_or_else(|| panic!("{method} {path} on {host} is not ambiguous"))
}

fn ref_of(host: &str, path: &str) -> String {
    git(host, "GET", path, Some("service=git-upload-pack")).describe()
}

#[test]
fn github_paths_name_owner_and_repository_the_way_the_host_reads_them() {
    for (path, repo) in [
        ("/acme/web.git/info/refs", "github.com/acme/web"),
        ("/acme/web/info/refs", "github.com/acme/web"),
        ("/Acme/Web.GIT/info/refs", "github.com/acme/web"),
        ("/ACME/WEB/INFO/REFS", "github.com/acme/web"),
        ("/acme/web.git.git/info/refs", "github.com/acme/web.git"),
    ] {
        assert_eq!(ref_of("github.com", path), repo, "{path}");
    }
}

#[test]
fn nested_groups_are_part_of_the_repository_on_a_host_that_has_them() {
    for (path, query, repo, access) in [
        (
            "/Group/Sub/Proj.git/info/refs",
            Some("service=git-receive-pack"),
            "gitlab.example/group/sub/proj",
            Access::Push,
        ),
        (
            "/g/s1/s2/s3/p/git-upload-pack",
            None,
            "gitlab.example/g/s1/s2/s3/p",
            Access::Pull,
        ),
        (
            "/g/s/p/info/lfs/objects/batch",
            None,
            "gitlab.example/g/s/p",
            Access::Batch,
        ),
    ] {
        let method = if path.ends_with("info/refs") {
            "GET"
        } else {
            "POST"
        };
        let found = git("gitlab.example", method, path, query);
        assert_eq!(found.describe(), repo, "{path}");
        assert_eq!(found.access, access, "{path}");
        assert_eq!(found.owner, repo.split('/').nth(1).unwrap());
    }
    // A project directly under a top group is the flat shape, as before.
    assert_eq!(
        ref_of("gitlab.example", "/g/p/info/refs"),
        "gitlab.example/g/p"
    );
}

#[test]
fn a_nested_read_needs_the_services_own_words() {
    for path in [
        "/g/s/p/info/refs",
        "/g/s/p/blob/main/info/refs",
        "/g/s/-/blob/main/git-upload-pack",
        "/g/s/-/info/lfs/objects/batch",
        "/g/s/p/info",
    ] {
        assert_eq!(
            classify("gitlab.example", "GET", path, None),
            Classified::NotGit,
            "{path}"
        );
    }
    // github.com has no groups: the flat shape only.
    assert_eq!(
        classify("github.com", "POST", "/acme/web/more/git-upload-pack", None),
        Classified::NotGit
    );
}

#[test]
fn a_nested_path_with_two_readings_or_too_many_names_is_ambiguous() {
    for path in [
        "/g/p/info/lfs/info/refs",
        "/g/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/git-upload-pack",
    ] {
        let got = classify(
            "gitlab.example",
            "GET",
            path,
            Some("service=git-upload-pack"),
        );
        assert!(matches!(got, Classified::Ambiguous(_)), "{path}: {got:?}");
    }
}

#[test]
fn an_azure_devops_path_that_stops_at_git_names_no_repository() {
    for (host, path) in [
        ("dev.azure.com", "/org/proj/_git"),
        ("dev.azure.com", "/org/_git"),
        ("contoso.visualstudio.com", "/proj/_git"),
        ("contoso.visualstudio.com", "/DefaultCollection/proj/_git"),
    ] {
        assert_eq!(
            classify(host, "GET", path, None),
            Classified::NotGit,
            "{path}"
        );
    }
}

#[test]
fn azure_devops_paths_in_every_shape() {
    for (host, path, repo) in [
        (
            "dev.azure.com",
            "/Org/Proj/_git/Repo/info/refs",
            "dev.azure.com/org/proj/repo",
        ),
        (
            "dev.azure.com",
            "/org/_GIT/repo/info/refs",
            "dev.azure.com/org/repo/repo",
        ),
        (
            "contoso.visualstudio.com",
            "/proj/_git/repo/info/refs",
            "contoso.visualstudio.com/contoso/proj/repo",
        ),
        (
            "contoso.visualstudio.com",
            "/DefaultCollection/proj/_git/repo/info/refs",
            "contoso.visualstudio.com/contoso/proj/repo",
        ),
    ] {
        let found = git(host, "GET", path, Some("service=git-upload-pack"));
        assert_eq!(found.describe(), repo, "{path}");
        assert_eq!(found.credential_host, "dev.azure.com");
    }
    // The table spells a repository by its host, owner and `project/repo`.
    let found = git(
        "dev.azure.com",
        "GET",
        "/org/proj/_git/repo.git/info/refs",
        None,
    );
    let table = found.repo_ref().unwrap();
    assert_eq!(table.to_string(), "dev.azure.com/org/proj/repo");
    // Not a repository path: no `_git`, an unknown collection.
    for (host, path) in [
        ("dev.azure.com", "/org/proj/repo/info/refs"),
        ("dev.azure.com", "/org/_apis/git/repositories"),
        (
            "contoso.visualstudio.com",
            "/Other/proj/_git/repo/info/refs",
        ),
        ("contoso.visualstudio.com", "/info/refs"),
    ] {
        assert_eq!(
            classify(host, "GET", path, None),
            Classified::NotGit,
            "{path}"
        );
    }
}

#[test]
fn the_credential_host_of_every_azure_devops_name_is_one() {
    let on_visualstudio = git(
        "contoso.visualstudio.com",
        "GET",
        "/p/_git/r/git-upload-pack",
        None,
    );
    assert_eq!(
        (
            on_visualstudio.credential_host.as_str(),
            on_visualstudio.owner.as_str()
        ),
        ("dev.azure.com", "contoso")
    );
    assert_eq!(on_visualstudio.host, "contoso.visualstudio.com");
    let elsewhere = git("git.example", "POST", "/team/app/git-upload-pack", None);
    assert_eq!(elsewhere.credential_host, "git.example");
}

#[test]
fn what_each_endpoint_asks_for() {
    use Access::{Batch, Pull, Push};
    let cases: [(&str, &str, &str, Option<&str>, Access); 24] = [
        (
            "GET",
            "info/refs",
            "",
            Some("service=git-upload-pack"),
            Pull,
        ),
        (
            "GET",
            "info/refs",
            "",
            Some("service=git-receive-pack"),
            Push,
        ),
        ("GET", "info/refs", "", None, Pull),
        (
            "GET",
            "info/refs",
            "",
            Some("service=git-upload-archive"),
            Push,
        ),
        (
            "POST",
            "info/refs",
            "",
            Some("service=git-upload-pack"),
            Push,
        ),
        ("POST", "git-upload-pack", "", None, Pull),
        ("GET", "git-upload-pack", "", None, Pull),
        ("PUT", "git-upload-pack", "", None, Push),
        ("POST", "git-receive-pack", "", None, Push),
        ("GET", "git-receive-pack", "", None, Push),
        // `service=git-receive-pack` anywhere is a push.
        (
            "POST",
            "git-upload-pack",
            "",
            Some("service=git-receive-pack"),
            Push,
        ),
        ("POST", "info/lfs/objects/batch", "", None, Batch),
        ("GET", "info/lfs/objects/batch", "", None, Pull),
        ("PUT", "info/lfs/objects/batch", "", None, Push),
        ("POST", "info/lfs/objects/verify", "", None, Push),
        ("GET", "info/lfs/objects/abc123", "", None, Pull),
        ("PUT", "info/lfs/objects/abc123", "", None, Push),
        ("GET", "info/lfs/locks", "", None, Pull),
        ("POST", "info/lfs/locks", "", None, Push),
        ("POST", "info/lfs/locks/verify", "", None, Pull),
        ("POST", "info/lfs/locks/42/unlock", "", None, Push),
        ("DELETE", "info/lfs", "", None, Push),
        // The word of a push in any later segment makes it a push.
        ("GET", "info/lfs/git-receive-pack", "", None, Push),
        ("GET", "info/lfs/objects/git-receive-pack", "", None, Push),
    ];
    for (method, endpoint, _, query, want) in cases {
        let path = format!("/acme/web.git/{endpoint}");
        assert_eq!(
            git("github.com", method, &path, query).access,
            want,
            "{method} {endpoint} {query:?}"
        );
    }
}

#[test]
fn two_service_parameters_or_a_hidden_one_are_ambiguous_or_a_push() {
    let path = "/acme/web.git/info/refs";
    assert_eq!(
        classify(
            "github.com",
            "GET",
            path,
            Some("service=git-upload-pack&service=git-receive-pack")
        ),
        Classified::Ambiguous("more than one service in the query")
    );
    for query in [
        "service=git%2Dreceive%2Dpack",
        "SERVICE=git-receive-pack",
        "servic%65=git-receive-pack",
        "x=1;service=git-receive-pack",
        "service=GIT-RECEIVE-PACK",
    ] {
        assert_eq!(
            git("github.com", "GET", path, Some(query)).access,
            Access::Push,
            "{query}"
        );
    }
}

#[test]
fn a_path_that_is_not_a_git_request_is_none_of_our_business() {
    for (host, path) in [
        ("github.com", "/"),
        ("github.com", "/acme"),
        ("github.com", "/acme/web"),
        ("github.com", "/acme/web/blob/main/info/refs"),
        ("github.com", "/acme/web/pull/1/files"),
        ("github.com", "/login/oauth/access_token"),
        ("github.com", "/acme/web.git/HEAD"),
        ("github.com", "/acme/web.git/info/refsx"),
        ("github.com", "/acme/web.git/x/git-receive-pack"),
        // Odd, but no reading makes it a Git request.
        ("github.com", "/a//b"),
        ("github.com", "/a/../b"),
        ("github.com", "/a%2Fb/c"),
        ("github.com", "/a/b;x=1"),
    ] {
        assert_eq!(
            classify(host, "GET", path, None),
            Classified::NotGit,
            "{host}{path}"
        );
    }
    assert_eq!(git_of(classify("github.com", "GET", "/", None)), None);
    assert_eq!(why(&classify("github.com", "GET", "/", None)), None);
}

// The bypass probes of the first credential-injection test on a real host: every spelling
// that could let a push reach a repository the table does not list.

#[test]
fn probe_dot_segments_are_refused() {
    for path in [
        "/acme/web.git/../other.git/git-receive-pack",
        "/acme/web.git/./info/refs",
        "/acme/web.git/info/./refs",
        "/acme/../other/web.git/info/refs",
        "/acme/web.git/%2e%2e/other.git/info/refs",
        "/acme/web.git/%2E%2E/other.git/info/refs",
        "/acme/web.git/.%2e/other.git/git-receive-pack",
    ] {
        assert!(
            why(&classify("github.com", "POST", path, None)).is_some(),
            "{path}"
        );
    }
    assert_eq!(
        ambiguous("github.com", "GET", "/acme/web.git/../x/info/refs"),
        "a dot segment"
    );
}

#[test]
fn probe_an_encoded_slash_is_refused() {
    for path in [
        "/acme%2Fother/web.git/info/refs",
        "/acme%2fother/web.git/info/refs",
        "/acme/web.git%2Finfo%2Frefs",
        "/acme/web.git/info%2Frefs",
        "/acme/web.git/git-receive-pack%2F",
        "/acme%2F..%2Fother/web.git/git-receive-pack",
    ] {
        assert!(
            why(&classify("github.com", "POST", path, None)).is_some(),
            "{path}"
        );
    }
}

#[test]
fn probe_double_slashes_and_trailing_slashes_are_refused() {
    for path in [
        "//acme/web.git/info/refs",
        "/acme//web.git/info/refs",
        "/acme/web.git//info/refs",
        "/acme/web.git/info//refs",
        "/acme/web.git/info/refs/",
        "/acme/web.git/git-receive-pack/",
    ] {
        assert!(
            why(&classify("github.com", "POST", path, None)).is_some(),
            "{path}"
        );
    }
}

#[test]
fn probe_backslashes_nul_and_control_bytes_are_refused() {
    for path in [
        "/acme\\other/web.git/info/refs",
        "/acme/web.git\\..\\other.git/git-receive-pack",
        "/acme/web.git/info/refs\\",
        "/acme/web.git/info/refs%5C",
        "/acme/web.git%5C..%5Cx/git-receive-pack",
        "/acme/web.git/git-receive-pack%00",
        "/acme/web.git/git-receive-pack\0",
        "/acme/web.git/git-receive-pack\n",
        "/acme/web.git/info/refs%0A",
    ] {
        assert!(
            why(&classify("github.com", "POST", path, None)).is_some(),
            "{path:?}"
        );
    }
}

#[test]
fn probe_non_canonical_percent_encoding_is_refused() {
    for path in [
        // An encoded letter, digit or `-._~`.
        "/acme/web.git/info/%72efs",
        "/acme/web.git/%67it-receive-pack",
        "/acme/we%62.git/git-receive-pack",
        "/acme/web%2Egit/git-receive-pack",
        "/acme/web.git/git%2Dreceive%2Dpack",
        // Lower-case hexadecimal, a bare or half escape, a double encoding.
        "/acme/web.git/info/refs%2f",
        "/acme/web.git/info/refs%",
        "/acme/web.git/info/refs%2",
        "/acme/web.git/info/refs%zz",
        "/acme/web.git/info/refs%252E",
        "/acme/web.git/%252e%252e/other/git-receive-pack",
        // Bytes that are not UTF-8 once decoded.
        "/acme/web.git/info/refs%C0%AE",
        "/acme/web.git/info/refs%FF",
    ] {
        assert!(
            why(&classify("github.com", "POST", path, None)).is_some(),
            "{path:?}"
        );
    }
}

#[test]
fn probe_path_parameters_and_raw_non_ascii_are_refused() {
    for path in [
        "/acme/web.git;x=1/git-receive-pack",
        "/acme/web.git/git-receive-pack;jsessionid=1",
        "/acme/web.git/info;/refs",
        "/acme/web.git/info/refs\u{e9}",
        "/acme/web.git/info/refs ",
        "/acme/web.git/info/refs\"",
        "/acme/{web}.git/info/refs",
        "/acme/web\u{2024}git/git-receive-pack",
    ] {
        assert!(
            why(&classify("github.com", "POST", path, None)).is_some(),
            "{path:?}"
        );
    }
}

#[test]
fn probe_case_and_the_git_suffix_do_not_make_another_repository() {
    let listed = ref_of("github.com", "/acme/web.git/info/refs");
    for path in [
        "/ACME/WEB/info/refs",
        "/acme/WEB.GIT/info/refs",
        "/Acme/Web/Info/Refs",
    ] {
        assert_eq!(ref_of("github.com", path), listed, "{path}");
    }
    // And a different repository stays different.
    assert_ne!(ref_of("github.com", "/acme/web2.git/info/refs"), listed);
    assert_ne!(ref_of("github.com", "/acme/web.git.git/info/refs"), listed);
}

#[test]
fn a_canonical_escape_for_a_byte_that_needs_one_is_read_into_the_table_spelling_and_a_name_the_table_cannot_hold_has_no_repo_ref()
 {
    let found = git(
        "dev.azure.com",
        "GET",
        "/org/My%20Project/_git/repo/info/refs",
        None,
    );
    assert_eq!(found.repo, "my project/repo");
    assert_eq!(
        found.repo_ref().unwrap().to_string(),
        "dev.azure.com/org/my project/repo"
    );
    // A name no host has (a character the table's alphabet leaves out) has no spelling.
    let odd = git("github.com", "GET", "/acme/a%2Bb/info/refs", None);
    assert_eq!(odd.repo, "a+b");
    assert!(odd.repo_ref().is_none());
}
