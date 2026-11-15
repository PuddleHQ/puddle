// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::WorkspaceId;

use super::*;

#[test]
fn a_layout_problem_keeps_its_reason() {
    let err = layout("no such place");
    assert!(
        matches!(&err, WorkspaceError::Layout { reason } if reason == "no such place"),
        "{err:?}"
    );
}

#[test]
fn the_origin_is_the_first_url_of_its_section_in_any_spelling() {
    let text = "\u{feff}[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = https://example.org/up.git\n\
                    [Remote \"origin\"]  # mine\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                    \tURL = \"https://github.com/acme/api.git\" ; comment\n\turl = https://second\n";
    assert_eq!(
        origin_url(text).as_deref(),
        Ok("https://github.com/acme/api.git")
    );
    assert_eq!(
        origin_url("[remote.origin]\nurl=git@github.com:acme/api.git\n").as_deref(),
        Ok("git@github.com:acme/api.git")
    );
    let none = |text: &str| origin_url(text).unwrap_err();
    // A line without an `=` and a header that never closes are skipped, not read.
    assert_eq!(
        origin_url("[remote \"origin\"]\n\tnot a setting\n\turl = https://x/y\n").as_deref(),
        Ok("https://x/y")
    );
    assert_eq!(
        none("[remote \"origin\"\nurl = https://x/y\n"),
        "it has no origin remote"
    );
    // The subsection name is exact; nothing else counts.
    assert_eq!(
        none("[remote \"Origin\"]\nurl = https://x/y\n"),
        "it has no origin remote"
    );
    assert_eq!(
        none("[remote \"origin\"] junk\nurl = https://x/y\n"),
        "it has no origin remote"
    );
    assert_eq!(
        none("[remote \"origin\"]\nfetch = x\n[core]\nurl = https://x/y\n"),
        "it has no origin remote"
    );
    assert_eq!(none("url = https://x/y\n"), "it has no origin remote");
    assert_eq!(none(""), "it has no origin remote");
}

#[test]
fn text_git_could_read_differently_is_refused_not_guessed_at() {
    for text in [
        // A continued line: git would read `url = ...` as part of `fetch`.
        "[remote \"origin\"]\n\tfetch = x \\\n\turl = https://evil.example/x.git\n",
        // A continued line in another section that swallows the header.
        "[core]\n\tfoo = bar\\\n[remote \"origin\"]\n\turl = https://evil.example/x.git\n",
        // An include before the origin may set it first.
        "[include]\n\tpath = other\n[remote \"origin\"]\n\turl = https://x/y\n",
        "[includeIf \"gitdir:~/\"]\n\tpath = other\n[remote \"origin\"]\n\turl = https://x/y\n",
        // Escapes this reader does not take.
        "[remote \"origin\"]\n\turl = https://x/\\ty\n",
        "[remote \"origin\"]\n\turl = \"unclosed\n",
    ] {
        assert_eq!(origin_url(text), Err(UNREADABLE.to_owned()), "{text}");
    }
    assert_eq!(config_value(r#""a\"b\\c""#).as_deref(), Some(r#"a"b\c"#));
    assert_eq!(
        config_value(r#""a # not a comment""#).as_deref(),
        Some("a # not a comment")
    );
    assert_eq!(config_value("  # only a comment"), None);
}

#[test]
fn ssh_remotes_of_known_hosts_get_their_https_form_and_credentials_go() {
    for (remote, https) in [
        (
            "https://github.com/acme/api.git",
            "https://github.com/acme/api.git",
        ),
        (
            "https://me:t0k3n@github.com/acme/api.git",
            "https://github.com/acme/api.git",
        ),
        (
            "HTTPS://me@dev.azure.com/org/p/_git/r",
            "https://dev.azure.com/org/p/_git/r",
        ),
        (
            "git@github.com:acme/api.git",
            "https://github.com/acme/api.git",
        ),
        (
            "ssh://git@ssh.github.com:443/acme/api.git",
            "https://github.com/acme/api.git",
        ),
        (
            "git@gitlab.com:group/sub/proj.git",
            "https://gitlab.com/group/sub/proj.git",
        ),
        (
            "git@bitbucket.org:ws/repo.git",
            "https://bitbucket.org/ws/repo.git",
        ),
        (
            "git@ssh.dev.azure.com:v3/org/proj/repo",
            "https://dev.azure.com/org/proj/_git/repo",
        ),
        (
            "ssh://org@vs-ssh.visualstudio.com/v3/org/proj/repo",
            "https://dev.azure.com/org/proj/_git/repo",
        ),
    ] {
        assert_eq!(https_form(remote).as_deref(), Ok(https), "{remote}");
    }
    for (remote, why) in [
        ("git@git.example.com:acme/api.git", "knows no HTTPS form"),
        (
            "git@ssh.dev.azure.com:v2/org/repo",
            "Azure DevOps SSH address",
        ),
        ("/home/u/other/api", "not an HTTPS or SSH address"),
        ("C:\\repos\\api", "not an HTTPS or SSH address"),
        ("http://example.org/api.git", "not an HTTPS or SSH address"),
        ("https://github.com/a b", "spaces or control characters"),
        ("https:///acme/api", "names no repository"),
        ("https://github.com", "names no repository"),
        ("git@github.com:acme/api?x", "SSH address Puddle can't read"),
    ] {
        let err = https_form(remote).unwrap_err();
        assert!(err.contains(why), "{remote}: {err}");
    }
}

#[test]
fn the_host_is_where_git_and_curl_would_go_never_one_hidden_after_a_query() {
    // git and curl end the authority at `?`, `#` or `\`: the `@...` part is not a host.
    // Reading it any other way would aim origin at another host.
    for remote in [
        "https://github.com?@evil.example/acme/x.git",
        "https://evil.example#@github.com/x",
        "https://evil.example\\@github.com/x",
    ] {
        let err = https_form(remote).unwrap_err();
        assert!(err.contains("names no repository"), "{remote}: {err}");
    }
    let err = https_form("ssh://git@github.com?@evil.example/x").unwrap_err();
    assert!(err.contains("SSH address Puddle can't read"), "{err}");
}

#[test]
fn a_refused_origin_never_shows_its_password() {
    for remote in [
        "http://me:s3cr3t@gitlab.internal/acme/x.git",
        "ssh://me:s3cr3t@git.example.com/acme/x.git",
        "https://me:s3cr3t@github.com",
    ] {
        let err = https_form(remote).unwrap_err();
        assert!(!err.contains("s3cr3t"), "{err}");
        assert!(err.contains("***@"), "{err}");
    }
}

fn git_dir(at: &Path) {
    std::fs::create_dir_all(at.join("objects")).unwrap();
    std::fs::create_dir_all(at.join("refs")).unwrap();
    std::fs::write(at.join("HEAD"), "ref: refs/heads/main\n").unwrap();
}

#[test]
fn a_checkout_is_found_from_its_folder_or_its_git_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let work = base.join("api");
    git_dir(&work.join(".git"));
    std::fs::write(
        work.join(".git").join("config"),
        "[remote \"origin\"]\n\turl = git@github.com:acme/api.git\n",
    )
    .unwrap();
    for picked in [
        work.clone(),
        work.join(".git"),
        base.join("api").join(".").join(".git"),
    ] {
        let c = LocalCheckout::open(&picked).unwrap();
        assert_eq!(c.git_dir, work.join(".git"));
        assert_eq!(c.folder, work);
        assert_eq!(c.origin.as_deref(), Ok("https://github.com/acme/api.git"));
    }
    std::fs::remove_file(work.join(".git").join("config")).unwrap();
    let c = LocalCheckout::open(&work).unwrap();
    assert!(c.origin.unwrap_err().contains("no config file"));
    std::fs::write(work.join(".git").join("config"), "[core]\n").unwrap();
    assert!(
        LocalCheckout::open(&work)
            .unwrap()
            .origin
            .unwrap_err()
            .contains("no origin")
    );
}

#[test]
fn what_is_not_a_checkout_is_refused_with_the_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    let refused = |p: &Path, want: &str| {
        let err = LocalCheckout::open(p).unwrap_err();
        assert!(err.contains(want), "{}: {err}", p.display());
    };
    refused(Path::new("relative/api"), "not an absolute path");
    refused(&base.join("missing"), "can't be opened");
    std::fs::create_dir_all(base.join("plain")).unwrap();
    refused(&base.join("plain"), "has no .git");
    git_dir(&base.join("bare.git"));
    refused(&base.join("bare.git"), "bare repository");
    let linked = base.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::write(linked.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
    refused(&linked, "linked worktree or a submodule");
    let empty = base.join("empty");
    std::fs::create_dir_all(empty.join(".git")).unwrap();
    refused(&empty, "is not a git directory");
    let shared = base.join("shared").join(".git");
    git_dir(&shared);
    std::fs::create_dir_all(shared.join("objects").join("info")).unwrap();
    std::fs::write(
        shared.join("objects").join("info").join("alternates"),
        "/elsewhere/objects\n",
    )
    .unwrap();
    refused(&base.join("shared"), "alternates");
    let partial = base.join("partial").join(".git");
    git_dir(&partial);
    std::fs::write(
            partial.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/a/b\n\tpromisor = true\n\tpartialclonefilter = blob:none\n",
        )
        .unwrap();
    refused(&base.join("partial"), "partial clone");
}

#[test]
fn a_config_that_is_too_large_or_not_a_file_gives_no_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let git = tmp.path().join(".git");
    git_dir(&git);
    std::fs::create_dir(git.join("config")).unwrap();
    assert!(
        read_config(&git.join("config"))
            .unwrap_err()
            .contains("no config file")
    );
    std::fs::remove_dir(git.join("config")).unwrap();
    let big = format!(
        "[remote \"origin\"]\n\turl = https://x/y\n{}",
        "#".repeat(usize::try_from(MAX_CONFIG_BYTES).unwrap())
    );
    std::fs::write(git.join("config"), big).unwrap();
    assert!(
        read_config(&git.join("config"))
            .unwrap_err()
            .contains("too large")
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(git.join("config")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", git.join("config")).unwrap();
        assert!(
            read_config(&git.join("config"))
                .unwrap_err()
                .contains("no config file")
        );
    }
}

#[test]
fn the_import_commands_are_argument_vectors_and_every_git_process_gets_the_guard() {
    let layout = Layout::new(&WorkspaceId::new("acme").unwrap()).unwrap();
    let dir = CheckoutDir::new("api").unwrap();
    assert_eq!(import_mount(2).unwrap().as_str(), "/puddle/import/2/.git");
    let clone = layout.import_request(2, &dir).unwrap();
    assert_eq!(clone.program, "git");
    assert_eq!(
        clone.args,
        [
            "clone",
            "--quiet",
            "--no-local",
            "--no-checkout",
            "--origin",
            "puddle-checkout",
            "--",
            "/puddle/import/2",
            "/workspaces/acme/api",
        ]
    );
    let env = import_env(2).unwrap();
    assert_eq!(clone.env, env);
    let pairs: Vec<(String, String)> = (0..env
        .get("GIT_CONFIG_COUNT")
        .unwrap()
        .parse::<usize>()
        .unwrap())
        .map(|i| {
            (
                env.get(&format!("GIT_CONFIG_KEY_{i}")).unwrap().to_owned(),
                env.get(&format!("GIT_CONFIG_VALUE_{i}"))
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    for (key, value) in [
        ("core.hooksPath", "/dev/null"),
        ("core.fsmonitor", "false"),
        ("safe.directory", "/puddle/import/2"),
        ("protocol.allow", "never"),
        ("protocol.file.allow", "always"),
        ("gc.auto", "0"),
        ("core.alternateRefsCommand", "true"),
    ] {
        assert!(
            pairs.contains(&(key.to_owned(), value.to_owned())),
            "{key}: {pairs:?}"
        );
    }
    assert_eq!(env.get("GIT_NO_LAZY_FETCH"), Some("1"));
    assert_eq!(env.get("GIT_LFS_SKIP_SMUDGE"), Some("1"));
    let url = "https://github.com/acme/a b'$(x)";
    let finish = layout.finish_import_request(2, &dir, url).unwrap();
    assert_eq!(finish.program, "sh");
    assert_eq!(finish.args[1], FINISH_IMPORT_SH);
    assert_eq!(
        finish.args[3..],
        ["/workspaces/acme/api", "/puddle/import/2", url]
    );
    assert_eq!(finish.env, env);
}
