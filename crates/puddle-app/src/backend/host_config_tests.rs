// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_hosts_config_takes_the_port_data_folder_agent_and_connection_file_from_the_options() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options {
        port: 4711,
        data_dir: Some(dir.path().to_path_buf()),
        agent: Some(dir.path().join("agent")),
        connection_file: Some(dir.path().join("connection.json")),
        ..Options::default()
    };
    let config = host_config(&options).unwrap();
    assert_eq!(config.api.port, 4711);
    assert_eq!(config.guest.agent_binary, dir.path().join("agent"));
    assert_eq!(
        config.api.connection_file,
        Some(dir.path().join("connection.json"))
    );
}
