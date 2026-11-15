// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_path_is_below_the_data_folder() {
    let paths = HostPaths::new("/data/puddle");
    for path in [
        paths.store(),
        paths.settings(),
        paths.workspace_book(),
        paths.quit_trims(),
        paths.guest_share(),
        paths.front_keys(),
        paths.front_port_file(),
        paths.connection_file(),
    ] {
        assert!(path.starts_with("/data/puddle"), "{path:?}");
    }
    assert_eq!(paths.data(), Path::new("/data/puddle"));
}

#[test]
fn the_names_are_distinct() {
    let paths = HostPaths::new("/d");
    let all = [
        paths.store(),
        paths.settings(),
        paths.workspace_book(),
        paths.quit_trims(),
        paths.guest_share(),
        paths.front_keys(),
        paths.front_port_file(),
        paths.connection_file(),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in all.iter().skip(i + 1) {
            assert_ne!(a, b);
        }
    }
}
