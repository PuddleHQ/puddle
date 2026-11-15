// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_defaults_follow_the_data_folder() {
    // Absolute on every platform ("/rt" has no drive on Windows).
    let layout = RuntimeLayout::new(
        std::path::absolute("/rt").unwrap(),
        std::path::absolute("/data/msb").unwrap(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let config = HostConfig::new(
        HostPaths::new("/data"),
        layout,
        GuestSettings::new("/rt/puddle-agent"),
    );
    assert_eq!(
        config.api.connection_file.as_deref(),
        Some(Path::new("/data/api.json"))
    );
    assert_eq!(config.api.port, 0);
    assert_eq!(config.expected_runtime, RuntimeVersion::built_for());
    assert!(config.guest.git_identity.is_none());
    assert!(format!("{:?}", config.api).contains("connection_file"));
}

#[test]
fn the_installed_layout_keeps_state_in_the_given_folder_and_the_runtime_beside_the_exe() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    let data = dir.path().join("data");
    let config = HostConfig::installed(&app.join("puddle-app"), &data).unwrap();
    assert_eq!(config.paths.data(), data);
    assert_eq!(config.layout.runtime_dir(), app.join("runtime"));
    assert_eq!(
        config.guest.agent_binary,
        app.join("runtime").join(AGENT_FILE_NAME)
    );
    assert_eq!(config.guest.code_server, None);
    assert_eq!(config.api.connection_file, Some(data.join("api.json")));
}
