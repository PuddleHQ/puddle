// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::path::PathBuf;

fn layout() -> RuntimeLayout {
    let root = std::path::absolute("env-test").unwrap();
    RuntimeLayout::new(root.join("rt"), root.join("home")).unwrap()
}

fn vars(names: &[&str]) -> Vec<(OsString, OsString)> {
    names
        .iter()
        .map(|n| (OsString::from(n), OsString::from("/user/value")))
        .collect()
}

#[test]
fn every_msb_variable_is_removed_and_nothing_else() {
    let env = RuntimeEnv::plan(
        &layout(),
        vars(&[
            "PATH",
            "MSB_PATH",
            "MSB_LIBKRUNFW_PATH",
            "MSB_AGENTD_PATH",
            "MSB_BACKEND",
            "msb_home",
            "Msb_Api_Key",
            "MSBX",
            "MY_MSB_PATH",
            "HOME",
        ]),
    );
    let removed: Vec<_> = env.removed().iter().map(|n| n.to_str().unwrap()).collect();
    assert_eq!(
        removed,
        [
            "MSB_AGENTD_PATH",
            "MSB_BACKEND",
            "MSB_LIBKRUNFW_PATH",
            "MSB_PATH",
            "Msb_Api_Key",
            "msb_home"
        ]
    );
}

#[test]
fn sets_exactly_path_home_and_config_to_absolute_paths() {
    let l = layout();
    let env = RuntimeEnv::plan(&l, vars(&["MSB_PATH"]));
    let set: Vec<_> = env
        .set()
        .iter()
        .map(|(n, v)| (*n, PathBuf::from(v)))
        .collect();
    assert_eq!(
        set,
        [
            ("MSB_PATH", l.msb_path()),
            ("MSB_HOME", l.home().to_path_buf()),
            ("MSB_CONFIG_PATH", l.config_path()),
        ]
    );
    assert!(set.iter().all(|(_, p)| p.is_absolute()));
    // The firmware and agent paths stay unset: msb finds them next to its own binary.
    assert!(
        env.set()
            .iter()
            .all(|(n, _)| *n != "MSB_LIBKRUNFW_PATH" && *n != "MSB_AGENTD_PATH")
    );
}

#[test]
fn prefix_match_is_exact_about_the_underscore() {
    assert!(is_msb_variable(OsStr::new("MSB_")));
    assert!(is_msb_variable(OsStr::new("msb_x")));
    assert!(!is_msb_variable(OsStr::new("MSB")));
    assert!(!is_msb_variable(OsStr::new("MSBPATH")));
    assert!(!is_msb_variable(OsStr::new("")));
}

#[test]
fn command_gets_the_plan() {
    let env = RuntimeEnv::plan(&layout(), vars(&["MSB_BACKEND"]));
    let mut cmd = Command::new("msb");
    env.apply_to(&mut cmd);
    let envs: Vec<_> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_str().unwrap().to_owned(), v.is_some()))
        .collect();
    assert!(envs.contains(&("MSB_BACKEND".into(), false)));
    assert!(envs.contains(&("MSB_PATH".into(), true)));
    assert!(envs.contains(&("MSB_HOME".into(), true)));
    assert!(envs.contains(&("MSB_CONFIG_PATH".into(), true)));
}
