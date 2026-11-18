// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn parses_the_submodule_commit() {
    let sha = "21f169f7e94798c8916315f8ac6f5999d5b88566";
    assert_eq!(
        parse_submodule_commit(&format!("160000 commit {sha}\tvendor/libkrunfw\n")).unwrap(),
        sha
    );
    assert!(parse_submodule_commit("").is_err());
    assert!(parse_submodule_commit(&format!("100644 blob {sha}\tvendor/libkrunfw")).is_err());
    assert!(parse_submodule_commit("160000 commit abc\tvendor/libkrunfw").is_err());
}

#[test]
fn parses_the_submodule_url() {
    let modules = "[submodule \"vendor/libkrunfw\"]\n\tpath = vendor/libkrunfw\n\turl = https://github.com/superradcompany/libkrunfw.git\n\tbranch = krunfw\n[submodule \"mcp\"]\n\tpath = mcp\n\turl = https://github.com/x/mcp.git\n";
    assert_eq!(
        parse_submodule_url(modules, "vendor/libkrunfw").unwrap(),
        "https://github.com/superradcompany/libkrunfw.git"
    );
    assert_eq!(
        parse_submodule_url(modules, "mcp").unwrap(),
        "https://github.com/x/mcp.git"
    );
    assert!(parse_submodule_url(modules, "other").is_err());
    // url before path in the section still counts
    let reordered = "[submodule \"a\"]\n url = https://github.com/o/a\n path = a\n";
    assert_eq!(
        parse_submodule_url(reordered, "a").unwrap(),
        "https://github.com/o/a"
    );
}

#[test]
fn github_slugs() {
    assert_eq!(github_slug("https://github.com/o/r.git").unwrap(), "o/r");
    assert_eq!(github_slug("https://github.com/o/r/").unwrap(), "o/r");
    assert!(github_slug("git@github.com:o/r.git").is_err());
    assert!(github_slug("https://github.com/o/r/x").is_err());
    assert!(github_slug("https://github.com/../r").is_err());
}

#[test]
fn msb_is_resolved_for_the_target_of_each_os() {
    assert_eq!(msb_target(HostOs::Windows), "x86_64-pc-windows-msvc");
    assert_eq!(msb_target(HostOs::Linux), "x86_64-unknown-linux-gnu");
}

#[test]
fn parses_libkrunfw_makefile() {
    let mk = "KERNEL_VERSION = linux-6.12.109\nKERNEL_REMOTE = x\nABI_VERSION = 5\nFULL_VERSION = 5.6.1\n";
    assert_eq!(
        parse_libkrunfw_makefile(mk).unwrap(),
        ("5.6.1".into(), "6.12.109".into())
    );
    assert!(
        parse_libkrunfw_makefile("FULL_VERSION = 1")
            .unwrap_err()
            .to_string()
            .contains("KERNEL_VERSION")
    );
    assert!(parse_libkrunfw_makefile("KERNEL_VERSION = 1").is_err());
}

#[test]
fn parses_compare_responses() {
    let json = r#"{"total_commits": 2, "commits": [
            {"sha": "aaa", "commit": {"message": "fix(vsock): one\n\nbody"}},
            {"sha": "bbb", "commit": {"message": ""}}
        ]}"#;
    let page = parse_compare(json).unwrap();
    assert_eq!(page.total, 2);
    let commits = page.commits;
    assert_eq!(
        commits[0],
        Commit {
            sha: "aaa".into(),
            subject: "fix(vsock): one".into()
        }
    );
    assert_eq!(commits[1].subject, "");
    assert!(parse_compare("{}").is_err());
}

fn page(total: usize, shas: &[&str]) -> String {
    let commits: Vec<String> = shas
        .iter()
        .map(|s| format!(r#"{{"sha":"{s}","commit":{{"message":"m {s}"}}}}"#))
        .collect();
    format!(
        r#"{{"total_commits":{total},"commits":[{}]}}"#,
        commits.join(",")
    )
}

#[test]
fn a_comparison_longer_than_one_page_is_read_to_the_end() {
    let pages = [page(3, &["a", "b"]), page(3, &["c"])];
    let commits = all_commits(|n| Ok(pages[n as usize - 1].clone())).unwrap();
    let shas: Vec<&str> = commits.iter().map(|c| c.sha.as_str()).collect();
    assert_eq!(shas, ["a", "b", "c"]);
}

#[test]
fn a_comparison_that_stays_short_of_its_total_is_an_error() {
    let pages = [page(3, &["a", "b"]), page(3, &[])];
    let err = all_commits(|n| Ok(pages[n as usize - 1].clone()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("2 commits against a total of 3"), "{err}");
    let too_many = all_commits(|_| Ok(page(1, &["a", "b"])))
        .unwrap_err()
        .to_string();
    assert!(
        too_many.contains("2 commits against a total of 1"),
        "{too_many}"
    );
}

fn github_source(firmware_tag: Option<&str>) -> GitHubSource {
    GitHubSource {
        fork: "f/m".into(),
        upstream: "u/m".into(),
        tag: "v1.0.0-puddle.1".into(),
        upstream_tag: "v1.0.0".into(),
        firmware_tag: firmware_tag.map(ToOwned::to_owned),
        work: PathBuf::from("/w"),
        about_config: PathBuf::from("/c"),
        cargo: Cargo::from_env(true),
        github: GitHub::public(),
    }
}

#[test]
fn github_source_maps_origins_to_repos() {
    let s = github_source(Some("v5.6.1-puddle.1"));
    assert_eq!(s.repo(Origin::Fork).unwrap(), ("f/m", "v1.0.0-puddle.1"));
    assert_eq!(s.repo(Origin::Upstream).unwrap(), ("u/m", "v1.0.0"));
    assert_eq!(
        s.repo(Origin::FirmwareFork).unwrap(),
        (FIRMWARE_FORK, "v5.6.1-puddle.1")
    );
    let err = github_source(None)
        .repo(Origin::FirmwareFork)
        .unwrap_err()
        .to_string();
    assert!(err.contains("libkrunfw fork"), "{err}");
}

#[test]
fn the_compare_base_names_upstreams_owner() {
    assert_eq!(
        compare_path(
            "PuddleHQ/microsandbox",
            "superradcompany/microsandbox",
            "v0.7.7",
            "v0.7.7-puddle.3"
        ),
        "repos/PuddleHQ/microsandbox/compare/superradcompany:v0.7.7...v0.7.7-puddle.3"
    );
}

#[test]
fn a_commit_response_gives_its_sha() {
    assert_eq!(
        parse_commit_sha(r#"{"sha":"abc123","x":1}"#).unwrap(),
        "abc123"
    );
    assert!(parse_commit_sha("{}").is_err());
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const MAKEFILE: &[u8] = b"KERNEL_VERSION = linux-6.12.109\nFULL_VERSION = 5.6.1\n";
const GITMODULES: &str = "[submodule \"vendor/libkrunfw\"]\n\tpath = vendor/libkrunfw\n\turl = https://github.com/x/krunfw.git\n";

fn routes(fork_commit: &str) -> Vec<(String, Vec<u8>)> {
    let makefile_path = format!("/repos/PuddleHQ/libkrunfw/contents/Makefile?ref={SHA}");
    vec![
        (
            "/repos/f/m/compare/u:v1.0.0...v1.0.0-puddle.1".into(),
            br#"{"total_commits":2,"commits":[{"sha":"aaa","commit":{"message":"fix: one"}}]}"#
                .to_vec(),
        ),
        (
            "/repos/f/m/compare/u:v1.0.0...v1.0.0-puddle.1?page=2".into(),
            br#"{"total_commits":2,"commits":[{"sha":"bbb","commit":{"message":"fix: two"}}]}"#
                .to_vec(),
        ),
        (
            "/repos/f/m/commits/v1.0.0-puddle.1".into(),
            format!(r#"{{"sha":"{fork_commit}"}}"#).into_bytes(),
        ),
        (
            "/repos/PuddleHQ/libkrunfw/commits/v5.6.1-puddle.1".into(),
            format!(r#"{{"sha":"{SHA}"}}"#).into_bytes(),
        ),
        (makefile_path, MAKEFILE.to_vec()),
        (
            format!("/repos/x/krunfw/contents/Makefile?ref={SHA}"),
            MAKEFILE.to_vec(),
        ),
        (
            "/f/m/releases/download/v1.0.0-puddle.1/msb".into(),
            b"bin".to_vec(),
        ),
    ]
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    run(git_command()
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args))
    .unwrap()
}

/// A repository at `dir` with the fork's files in one commit, tagged `tag`; returns the commit.
fn fork_repo(dir: &Path, tag: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(dir.join(".gitmodules"), GITMODULES).unwrap();
    git_in(dir, &["init", "--quiet"]);
    git_in(dir, &["add", "Cargo.toml", ".gitmodules"]);
    git_in(
        dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{SHA},vendor/libkrunfw"),
        ],
    );
    git_in(dir, &["commit", "--quiet", "-m", "x"]);
    git_in(dir, &["tag", tag]);
    git_in(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// A source reading from a local server, with the fork's source already cloned under `work` at
/// the commit the served tag names.
fn served_source(firmware_tag: Option<&str>, work: &Path) -> GitHubSource {
    let src = work.join("src-v1.0.0-puddle.1");
    let commit = fork_repo(&src, "v1.0.0-puddle.1");
    served_source_at(firmware_tag, work, &commit)
}

/// Serves `routes` (leaked: the server thread outlives the test and wants `'static` ones).
fn serve_routes(routes: Vec<(String, Vec<u8>)>) -> String {
    let routes: Vec<(&'static str, &'static [u8])> = routes
        .into_iter()
        .map(|(path, body)| {
            (
                &*Box::leak(path.into_boxed_str()),
                &*Box::leak(body.into_boxed_slice()),
            )
        })
        .collect();
    crate::github::tests::serve(Box::leak(routes.into_boxed_slice()))
}

/// As [`served_source`], with the served tag naming `fork_commit`.
fn served_source_at(firmware_tag: Option<&str>, work: &Path, fork_commit: &str) -> GitHubSource {
    let base = serve_routes(routes(fork_commit));
    let mut source = github_source(firmware_tag);
    source.github = GitHub::at(&base, &base, "curl");
    source.work = work.to_path_buf();
    source
}

#[test]
fn facts_of_a_pinned_firmware_release_come_from_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let facts = served_source(Some("v5.6.1-puddle.1"), dir.path())
        .facts()
        .unwrap();
    assert_eq!(facts.commits.len(), 2);
    assert_eq!(facts.firmware.commit, SHA);
    assert_eq!(facts.firmware.libkrunfw_version, "5.6.1");
    assert_eq!(facts.firmware.kernel_version, "6.12.109");
    assert_eq!(facts.firmware.fork_tag.as_deref(), Some("v5.6.1-puddle.1"));
}

#[test]
fn facts_of_upstreams_firmware_follow_the_submodule_of_the_fork_source() {
    let dir = tempfile::tempdir().unwrap();
    let facts = served_source(None, dir.path()).facts().unwrap();
    assert_eq!(facts.firmware.commit, SHA);
    assert_eq!(facts.firmware.repo_url, "https://github.com/x/krunfw");
    assert_eq!(facts.firmware.fork_tag, None);
}

#[test]
fn a_cached_clone_that_is_not_the_tag_is_cloned_again() {
    let dir = tempfile::tempdir().unwrap();
    // The cache holds a commit the tag no longer names; the tag now names the one in `origin`.
    let stale = dir.path().join("work/src-v1.0.0-puddle.1");
    std::fs::create_dir_all(&stale).unwrap();
    let stale_commit = fork_repo(&stale, "v1.0.0-puddle.1");
    let origin = dir.path().join("web/f/m.git");
    std::fs::create_dir_all(&origin).unwrap();
    let moved = fork_repo(&origin, "v1.0.0-puddle.1");
    std::fs::write(origin.join("moved"), "").unwrap();
    git_in(&origin, &["add", "moved"]);
    git_in(&origin, &["commit", "--quiet", "-m", "moved"]);
    git_in(&origin, &["tag", "-f", "v1.0.0-puddle.1"]);
    let tagged = git_in(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_ne!(tagged, stale_commit);
    assert_ne!(moved, tagged);

    let mut source = served_source_at(None, &dir.path().join("work"), &tagged);
    source.github = GitHub::at(
        &source.github.api_root(),
        &format!("file://{}", dir.path().join("web").display()),
        "curl",
    );
    let src = source.checkout().unwrap();
    assert_eq!(git_in(&src, &["rev-parse", "HEAD"]).trim(), tagged);
}

#[test]
fn a_cached_clone_at_the_tag_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let source = served_source(None, dir.path());
    let src = dir.path().join("src-v1.0.0-puddle.1");
    std::fs::write(src.join("marker"), "").unwrap();
    assert_eq!(source.checkout().unwrap(), src);
    assert!(src.join("marker").is_file());
}

#[test]
fn a_release_asset_is_downloaded_into_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let source = served_source(None, dir.path());
    let got = source.fetch(Origin::Fork, "msb", dir.path()).unwrap();
    assert_eq!(std::fs::read(got).unwrap(), b"bin");
}

#[test]
fn a_github_answer_that_is_missing_fails_the_facts() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = served_source(Some("v5.6.1-puddle.1"), dir.path());
    let commit = git_in(
        &dir.path().join("src-v1.0.0-puddle.1"),
        &["rev-parse", "HEAD"],
    );
    let mut no_makefile = routes(commit.trim());
    no_makefile.truncate(4);
    let base = serve_routes(no_makefile);
    source.github = GitHub::at(&base, &base, "curl");
    let err = source.facts().unwrap_err().to_string();
    assert!(err.contains("404") && err.contains("Makefile"), "{err}");

    let base = crate::github::tests::serve(&[]);
    source.github = GitHub::at(&base, &base, "curl");
    let err = source.facts().unwrap_err().to_string();
    assert!(err.contains("404") && err.contains("compare"), "{err}");
}
