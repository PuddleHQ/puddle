// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn settings(trusted: bool, image_path: Option<&str>) -> MiseSettings {
    MiseSettings {
        dir: GuestPath::new("/workspace/.puddle/mise").unwrap(),
        ceiling: GuestPath::new("/workspaces").unwrap(),
        trusted: trusted.then(|| GuestPath::new("/workspace").unwrap()),
        image_path: image_path.map(str::to_owned),
    }
}

fn text(config: &MiseConfig) -> &str {
    std::str::from_utf8(config.files[0].contents()).unwrap()
}

#[test]
fn trusted_workspace_gets_every_variable() {
    let config = mise_config(&settings(true, None)).unwrap();
    let env: Vec<(&str, &str)> = config.env.iter().collect();
    assert_eq!(
        env,
        [
            ("MISE_CACHE_DIR", "/workspace/.puddle/mise/cache"),
            ("MISE_CEILING_PATHS", "/workspaces"),
            ("MISE_DATA_DIR", "/workspace/.puddle/mise/data"),
            ("MISE_DISABLE_UPDATE_WARNING", "1"),
            ("MISE_STATE_DIR", "/workspace/.puddle/mise/state"),
            ("MISE_TRUSTED_CONFIG_PATHS", "/workspace"),
            ("MISE_USE_VERSIONS_HOST_TRACK", "0"),
            (
                "PATH",
                "/workspace/.puddle/mise/data/shims:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/workspace/.puddle/mise/bin"
            ),
        ]
    );
}

#[test]
fn without_trust_no_trusted_path_is_set() {
    let config = mise_config(&settings(false, None)).unwrap();
    assert_eq!(config.env.get("MISE_TRUSTED_CONFIG_PATHS"), None);
    assert!(!text(&config).contains("TRUSTED"));
    assert_eq!(config.env.get("MISE_CEILING_PATHS"), Some("/workspaces"));
}

#[test]
fn without_trust_mise_keeps_its_state_apart_from_the_trusted_runs() {
    // mise remembers every config it loaded as trusted in its state folder; a workspace
    // switched to no trust must not read those records.
    let on = mise_config(&settings(true, None)).unwrap();
    let off = mise_config(&settings(false, None)).unwrap();
    assert_eq!(
        on.env.get("MISE_STATE_DIR"),
        Some("/workspace/.puddle/mise/state")
    );
    assert_eq!(
        off.env.get("MISE_STATE_DIR"),
        Some("/workspace/.puddle/mise/state-untrusted")
    );
}

#[test]
fn the_image_path_sits_between_the_shims_and_the_binary() {
    let config = mise_config(&settings(true, Some("/usr/local/cargo/bin:/opt/x"))).unwrap();
    assert_eq!(
        config.env.get("PATH"),
        Some(
            "/workspace/.puddle/mise/data/shims:/usr/local/cargo/bin:/opt/x:/workspace/.puddle/mise/bin"
        )
    );
}

#[test]
fn sudo_keeps_the_mise_names_and_resets_to_the_same_path() {
    let config = mise_config(&settings(true, None)).unwrap();
    let file = &config.files[0];
    assert_eq!(file.path().as_str(), SUDOERS_MISE_GUEST);
    assert_eq!(file.mode(), 0o440);
    let sudoers = text(&config);
    assert!(sudoers.contains(
        "Defaults env_keep += \"MISE_CACHE_DIR MISE_CEILING_PATHS MISE_DATA_DIR MISE_DISABLE_UPDATE_WARNING MISE_STATE_DIR MISE_TRUSTED_CONFIG_PATHS MISE_USE_VERSIONS_HOST_TRACK\"\n"
    ));
    let path = config.env.get("PATH").unwrap();
    assert!(sudoers.contains(&format!("Defaults secure_path=\"{path}\"\n")));
}

#[test]
fn a_path_that_could_break_sudoers_leaves_secure_path_alone() {
    for bad in ["/a\"b", "/a b", "/a#b", "/a\\b", "/a$b"] {
        let config = mise_config(&settings(true, Some(bad))).unwrap();
        assert!(config.env.get("PATH").unwrap().contains(bad));
        assert!(!text(&config).contains("secure_path"), "{bad}");
    }
}

#[test]
fn a_nul_in_the_image_path_is_refused_with_its_name() {
    let err = mise_config(&settings(true, Some("/a\0b"))).unwrap_err();
    assert!(matches!(err, ConfigError::ImageEnv { ref name, .. } if name == "PATH"));
}
