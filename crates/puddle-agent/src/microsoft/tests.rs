// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const COMMIT: &str = "e646868227c3e3a9e3f24277c8c7220be42c0be7";

#[test]
fn a_refusal_says_what_is_wrong_without_the_checksums() {
    let checksum = InstallError::Checksum {
        expected: "a".repeat(64),
        actual: "b".repeat(64),
    };
    assert!(checksum.is_refusal());
    assert_eq!(
        checksum.refusal_summary(),
        "the download does not match the checksum Microsoft published for it"
    );
    let archive = InstallError::Archive("a link out of the folder".to_owned());
    assert_eq!(
        archive.refusal_summary(),
        "the VS Code server download holds a link out of the folder, which is not unpacked"
    );
}

#[test]
fn the_urls_are_the_ones_the_code_command_uses() {
    assert_eq!(
        latest_url("server-linux-x64-web"),
        "https://update.code.visualstudio.com/api/latest/server-linux-x64-web/stable"
    );
    assert_eq!(
        versions_url("1.143.1", "server-linux-x64-web"),
        "https://update.code.visualstudio.com/api/versions/1.143.1/server-linux-x64-web/stable"
    );
    assert_eq!(
        download_url(COMMIT, "server-linux-arm64-web"),
        format!(
            "https://update.code.visualstudio.com/commit:{COMMIT}/server-linux-arm64-web/stable"
        )
    );
}

#[test]
fn archive_paths_lose_their_top_folder_and_never_leave_the_build() {
    let below = |p: &str| below_top(Path::new(p)).map_err(|e| e.to_string());
    assert_eq!(
        below("top/bin/code-server"),
        Ok(Some(PathBuf::from("bin/code-server")))
    );
    assert_eq!(below("top/./a"), Ok(Some(PathBuf::from("a"))));
    assert_eq!(below("./top/a"), Ok(Some(PathBuf::from("a"))));
    // The top folder itself, and nothing at all.
    assert_eq!(below("top/"), Ok(None));
    assert_eq!(below(""), Ok(None));
    assert!(below("top/../../x").is_err());
    assert!(below("/etc/x").is_err());
}

#[test]
fn a_current_file_that_names_no_commit_names_no_build() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(current_build(dir.path()), None);
    std::fs::write(dir.path().join("current"), "../../etc").unwrap();
    assert_eq!(current_build(dir.path()), None);
}

#[test]
fn the_build_follows_the_machines_architecture() {
    assert_eq!(platform("x86_64").unwrap(), "server-linux-x64-web");
    assert_eq!(platform("aarch64").unwrap(), "server-linux-arm64-web");
    let err = platform("riscv64").unwrap_err();
    assert!(err.to_string().contains("riscv64"), "{err}");
}

#[test]
fn the_latest_answer_gives_the_commit_and_the_release() {
    let answer = format!(
        r#"{{"name":"1.143.1","version":"{COMMIT}","productVersion":"1.143.1","timestamp":1}}"#
    );
    assert_eq!(
        parse_latest(&answer).unwrap(),
        Latest {
            commit: COMMIT.to_owned(),
            product_version: "1.143.1".to_owned()
        }
    );
}

#[test]
fn what_goes_into_a_path_or_a_url_is_checked() {
    let with = |commit: &str, release: &str| {
        parse_latest(&format!(
            r#"{{"version":"{commit}","productVersion":"{release}"}}"#
        ))
    };
    assert!(with(COMMIT, "1.144.0-insider").is_ok());
    for (commit, release) in [
        ("../../x", "1.0.0"),
        (&COMMIT[..39], "1.0.0"),
        (&COMMIT.to_uppercase(), "1.0.0"),
        (COMMIT, "1.0/../0"),
        (COMMIT, ""),
        (COMMIT, "1 .0"),
    ] {
        assert!(with(commit, release).is_err(), "{commit} {release}");
    }
    assert!(parse_latest("{}").is_err());
    assert!(parse_latest("nope").is_err());
}

#[test]
fn the_published_hash_must_belong_to_the_build_asked_for() {
    let hash = "4626a9d6f036d382a77197f895eee35cce6843d0a7042ae8be850bec2c77188e";
    let answer = format!(r#"{{"version":"{COMMIT}","sha256hash":"{hash}","hash":"{COMMIT}"}}"#);
    assert_eq!(parse_published_hash(&answer, COMMIT).unwrap(), hash);
    assert!(parse_published_hash(&answer, &"1".repeat(40)).is_err());
    let no_hash = format!(r#"{{"version":"{COMMIT}"}}"#);
    assert!(parse_published_hash(&no_hash, COMMIT).is_err());
}

#[test]
fn the_command_line_carries_the_terms_the_token_file_and_the_telemetry_choice() {
    let server = EditorServer::microsoft("/w/.puddle/vscode-server", "/w", 120, false, true);
    let token = Path::new("/run/puddle/editor-password");
    let args = |telemetry: bool, target: &str| -> Vec<String> {
        arguments(&server, telemetry, token, OsStr::new(target))
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    };
    let off = args(false, "/w");
    let after = |flag: &str| {
        let at = off.iter().position(|a| a == flag).unwrap();
        off[at + 1].as_str()
    };
    assert_eq!(after("--host"), "127.0.0.1");
    assert_eq!(after("--port"), "41337");
    assert_eq!(
        after("--connection-token-file"),
        "/run/puddle/editor-password"
    );
    assert_eq!(after("--reconnection-grace-time"), "120");
    assert_eq!(after("--default-folder"), "/w");
    assert!(off.iter().any(|a| a == "--accept-server-license-terms"));
    assert!(off.iter().any(|a| a == "--disable-workspace-trust"));
    assert!(off.iter().any(|a| a == "--disable-telemetry"));
    // The data of the server and its extensions sit on the volume, in their own folders.
    let under = |leaf: &str| {
        Path::new("/w/.puddle/vscode-server")
            .join(leaf)
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(after("--server-data-dir"), under("server-data"));
    assert_eq!(after("--extensions-dir"), under("extensions"));
    assert!(!args(true, "/w").iter().any(|a| a == "--disable-telemetry"));
    // The multi-root file is a workspace, not a folder.
    let file = args(false, "/w/.puddle/editor/w.code-workspace");
    assert_eq!(
        file[file.len() - 2..],
        ["--default-workspace", "/w/.puddle/editor/w.code-workspace"]
    );
}
