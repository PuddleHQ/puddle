// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// An id the workbench of code-server 4.141 made itself: the folder name of the cached copy it
/// wrote for this multi-root file, opened at this window address.
const SEEN_URI: &str = "vscode-remote://r45854849579-1-wcs.localhost:43021/workspace/.puddle/editor/r45854849579-1-wcs.code-workspace";
const SEEN_ID: &str = "-ed3fb80";

#[test]
fn the_id_is_the_one_the_workbench_gave_the_same_file() {
    assert_eq!(workspace_id(SEEN_URI), SEEN_ID);
}

#[test]
fn a_positive_hash_has_no_sign_and_no_padding() {
    // hash("a") = (149417 * 31) + 97, wrapped to 32 bits.
    assert_eq!(workspace_id("a"), format!("{:x}", 149_417_i32 * 31 + 97));
}

#[test]
fn the_uri_is_written_as_the_workbench_writes_it() {
    assert_eq!(
        workspace_uri(
            "W.localhost:43021",
            "/workspace/.puddle/editor/w.code-workspace"
        )
        .as_deref(),
        Some("vscode-remote://w.localhost:43021/workspace/.puddle/editor/w.code-workspace")
    );
    assert_eq!(
        workspace_uri("w.localhost:1", "/a b/c.code-workspace"),
        None
    );
    assert_eq!(
        workspace_uri("w.localhost:1", "relative.code-workspace"),
        None
    );
    assert_eq!(workspace_uri("", "/w.code-workspace"), None);
}

fn server(file: &Path, authority: Option<&str>) -> EditorServer {
    let server =
        EditorServer::new("/data", "/workspace", 300).with_workspace_file(file.to_str().unwrap());
    match authority {
        Some(authority) => server.with_window_authority(authority),
        None => server,
    }
}

#[test]
fn fill_writes_the_file_text_under_the_windows_id() {
    let dir = tempfile::tempdir().unwrap();
    let user_data = dir.path().join("user-data");
    std::fs::create_dir(&user_data).unwrap();
    let file = dir.path().join("w.code-workspace");
    std::fs::write(
        &file,
        "{\n\t\"folders\": [{\"path\": \"/workspace/api\"}]\n}\n",
    )
    .unwrap();
    let authority = "w.localhost:43021";

    fill(&server(&file, Some(authority)), &user_data);

    let uri = format!("vscode-remote://{authority}{}", file.display());
    let entry = user_data
        .join(WORKSPACES_CACHE)
        .join(workspace_id(&uri))
        .join(ENTRY_FILE);
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&entry).unwrap()).unwrap();
    assert_eq!(
        written,
        serde_json::json!({"content": "{\n\t\"folders\": [{\"path\": \"/workspace/api\"}]\n}\n"})
    );
}

#[test]
fn fill_writes_nothing_without_a_window_address_or_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let user_data = dir.path().join("user-data");
    std::fs::create_dir(&user_data).unwrap();
    let file = dir.path().join("w.code-workspace");

    fill(&server(&file, Some("w.localhost:1")), &user_data);
    std::fs::write(&file, "{}").unwrap();
    fill(&server(&file, None), &user_data);

    assert!(!user_data.join("User").exists());
}

#[test]
fn fill_replaces_a_copy_of_an_older_file() {
    let dir = tempfile::tempdir().unwrap();
    let user_data = dir.path().join("user-data");
    std::fs::create_dir(&user_data).unwrap();
    let file = dir.path().join("w.code-workspace");
    let server = server(&file, Some("w.localhost:1"));
    std::fs::write(&file, "{\"folders\": []}").unwrap();
    fill(&server, &user_data);
    std::fs::write(&file, "{\"folders\": [{\"path\": \"/workspace\"}]}").unwrap();

    fill(&server, &user_data);

    let uri = format!("vscode-remote://w.localhost:1{}", file.display());
    let entry = entry_path(&user_data, "w.localhost:1", file.to_str().unwrap()).unwrap();
    assert!(entry.ends_with(format!("{}/{ENTRY_FILE}", workspace_id(&uri))));
    assert!(
        std::fs::read_to_string(entry)
            .unwrap()
            .contains("/workspace\\\"")
    );
}
