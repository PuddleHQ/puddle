// SPDX-License-Identifier: GPL-3.0-or-later
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
    let json = r#"{"commits": [
            {"sha": "aaa", "commit": {"message": "fix(vsock): one\n\nbody"}},
            {"sha": "bbb", "commit": {"message": ""}}
        ]}"#;
    let commits = parse_compare(json).unwrap();
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
const ROUTES: &[(&str, &[u8])] = &[
    (
        "/repos/f/m/compare/u:v1.0.0...v1.0.0-puddle.1",
        br#"{"commits":[{"sha":"aaa","commit":{"message":"fix: one"}}]}"#,
    ),
    (
        "/repos/PuddleHQ/libkrunfw/commits/v5.6.1-puddle.1",
        br#"{"sha":"0123456789abcdef0123456789abcdef01234567"}"#,
    ),
    (
        "/repos/PuddleHQ/libkrunfw/contents/Makefile?ref=0123456789abcdef0123456789abcdef01234567",
        MAKEFILE,
    ),
    (
        "/repos/x/krunfw/contents/Makefile?ref=0123456789abcdef0123456789abcdef01234567",
        MAKEFILE,
    ),
    ("/f/m/releases/download/v1.0.0-puddle.1/msb", b"bin"),
];

/// A source reading from a local server, with the fork's source already cloned under `work`.
fn served_source(firmware_tag: Option<&str>, work: &Path) -> GitHubSource {
    let base = crate::github::tests::serve(ROUTES);
    let mut source = github_source(firmware_tag);
    source.github = GitHub::at(&base, &base, "curl");
    source.work = work.to_path_buf();
    let src = work.join("src-v1.0.0-puddle.1");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("Cargo.toml"), "").unwrap();
    source
}

#[test]
fn facts_of_a_pinned_firmware_release_come_from_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let facts = served_source(Some("v5.6.1-puddle.1"), dir.path())
        .facts()
        .unwrap();
    assert_eq!(facts.commits.len(), 1);
    assert_eq!(facts.firmware.commit, SHA);
    assert_eq!(facts.firmware.libkrunfw_version, "5.6.1");
    assert_eq!(facts.firmware.kernel_version, "6.12.109");
    assert_eq!(facts.firmware.fork_tag.as_deref(), Some("v5.6.1-puddle.1"));
}

#[test]
fn facts_of_upstreams_firmware_follow_the_submodule_of_the_fork_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = served_source(None, dir.path());
    let src = dir.path().join("src-v1.0.0-puddle.1");
    std::fs::write(
        src.join(".gitmodules"),
        "[submodule \"vendor/libkrunfw\"]\n\tpath = vendor/libkrunfw\n\turl = https://github.com/x/krunfw.git\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        run(git_command()
            .arg("-C")
            .arg(&src)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args))
        .unwrap();
    };
    git(&["init", "--quiet"]);
    git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{SHA},vendor/libkrunfw"),
    ]);
    git(&["commit", "--quiet", "-m", "x"]);
    let facts = source.facts().unwrap();
    assert_eq!(facts.firmware.commit, SHA);
    assert_eq!(facts.firmware.repo_url, "https://github.com/x/krunfw");
    assert_eq!(facts.firmware.fork_tag, None);
}

#[test]
fn a_release_asset_is_downloaded_into_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let source = served_source(None, dir.path());
    let got = source.fetch(Origin::Fork, "msb", dir.path()).unwrap();
    assert_eq!(std::fs::read(got).unwrap(), b"bin");
}
