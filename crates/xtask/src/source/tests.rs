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
