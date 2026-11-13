// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use super::*;

fn read(vars: &[(&str, &str)]) -> Result<Option<EditorServer>, EditorEnvError> {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    EditorServer::from_lookup(|k| map.get(k).cloned())
}

#[test]
fn no_release_folder_means_no_server() {
    assert_eq!(read(&[]), Ok(None));
    assert_eq!(read(&[(ENV_PORT, "99999")]), Ok(None));
}

#[test]
fn the_host_s_variables_read_back_and_defaults_apply() {
    let server = EditorServer::new("/workspaces/a/.puddle/code-server", "/workspaces/a", 600);
    let env = server.env();
    let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(read(&vars), Ok(Some(server)));
    let minimal = read(&[(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")])
        .unwrap()
        .unwrap();
    assert_eq!((minimal.port, minimal.grace_secs), (DEFAULT_PORT, 300));
}

#[test]
fn the_session_user_reads_back_and_a_missing_one_is_root() {
    let server = EditorServer::new("/d", "/f", 300).with_user(SessionUser::Agent);
    let env = server.env();
    assert!(env.contains(&(ENV_USER, "agent".to_owned())));
    let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(read(&vars), Ok(Some(server)));
    let root = EditorServer::new("/d", "/f", 300);
    assert!(root.env().iter().all(|(k, _)| *k != ENV_USER));
    let base = [(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")];
    assert_eq!(read(&base).unwrap().unwrap().user, SessionUser::Root);
    let named = |user| read(&[base[0], base[1], base[2], (ENV_USER, user)]);
    assert_eq!(named("root").unwrap().unwrap().user, SessionUser::Root);
    assert_eq!(named("vscode").unwrap_err().var, ENV_USER);
}

#[test]
fn the_workspace_file_is_optional_and_reads_back() {
    let server = EditorServer::new("/d", "/f", 300).with_workspace_file("/f/w.code-workspace");
    let env = server.env();
    let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(read(&vars), Ok(Some(server)));
    let without = read(&[(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")]);
    assert_eq!(without.unwrap().unwrap().workspace_file, None);
}

#[test]
fn microsoft_s_server_keeps_its_builds_below_its_data_and_reads_back() {
    for (telemetry, auto_update) in [(false, true), (true, false), (false, false)] {
        let server = EditorServer::microsoft(
            "/workspaces/a/.puddle/vscode-server",
            "/workspaces/a",
            90,
            telemetry,
            auto_update,
        );
        assert_eq!(server.dir, "/workspaces/a/.puddle/vscode-server/servers");
        assert_eq!(
            server.kind,
            ServerKind::Microsoft {
                telemetry,
                auto_update
            }
        );
        let env = server.env();
        let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(read(&vars), Ok(Some(server)));
    }
    // code-server sends neither variable, and reads as code-server without them or named.
    let plain = EditorServer::new("/d", "/f", 300);
    assert!(
        plain
            .env()
            .iter()
            .all(|(k, _)| ![ENV_KIND, ENV_TELEMETRY, ENV_AUTO_UPDATE].contains(k))
    );
    let named = read(&[
        (ENV_DIR, "/e"),
        (ENV_DATA, "/d"),
        (ENV_FOLDER, "/f"),
        (ENV_KIND, "code-server"),
    ]);
    assert_eq!(named.unwrap().unwrap().kind, ServerKind::CodeServer);
}

#[test]
fn an_unknown_server_or_a_telemetry_flag_that_does_not_fit_names_its_variable() {
    let base = [(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")];
    let with = |extra: &[(&'static str, &'static str)]| {
        let mut v = base.to_vec();
        v.extend_from_slice(extra);
        read(&v).unwrap_err().to_string()
    };
    assert!(with(&[(ENV_KIND, "vscodium")]).starts_with(ENV_KIND));
    assert!(with(&[(ENV_KIND, "microsoft"), (ENV_TELEMETRY, "yes")]).starts_with(ENV_TELEMETRY));
    // Telemetry means something only for Microsoft's server: setting it for code-server is a mistake.
    assert!(with(&[(ENV_TELEMETRY, "1")]).starts_with(ENV_TELEMETRY));
    assert!(with(&[(ENV_AUTO_UPDATE, "0")]).starts_with(ENV_AUTO_UPDATE));
    assert!(
        with(&[(ENV_KIND, "microsoft"), (ENV_AUTO_UPDATE, "yes")]).starts_with(ENV_AUTO_UPDATE)
    );
    // Microsoft's server without the variables: telemetry off, updates on (an older host
    // sends no update setting).
    let off = read(&[
        (ENV_DIR, "/e"),
        (ENV_DATA, "/d"),
        (ENV_FOLDER, "/f"),
        (ENV_KIND, "microsoft"),
    ]);
    assert_eq!(
        off.unwrap().unwrap().kind,
        ServerKind::Microsoft {
            telemetry: false,
            auto_update: true
        }
    );
}

#[test]
fn unusable_values_name_their_variable() {
    let ok = [(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")];
    let with = |extra: (&'static str, &'static str)| {
        let mut v = ok.to_vec();
        v.retain(|(k, _)| *k != extra.0);
        v.push(extra);
        read(&v).unwrap_err().to_string()
    };
    assert!(with((ENV_PORT, "80")).starts_with(ENV_PORT));
    assert!(with((ENV_PORT, "x")).starts_with(ENV_PORT));
    assert!(with((ENV_GRACE, "29")).starts_with(ENV_GRACE));
    assert!(with((ENV_GRACE, "86401")).starts_with(ENV_GRACE));
    assert!(with((ENV_DATA, "relative")).starts_with(ENV_DATA));
    assert!(with((ENV_FOLDER, "/a/../b")).starts_with(ENV_FOLDER));
    assert!(with((ENV_WORKSPACE_FILE, "w.code-workspace")).starts_with(ENV_WORKSPACE_FILE));
    assert!(with((ENV_DIR, "e")).starts_with(ENV_DIR));
    for missing in [ENV_DATA, ENV_FOLDER] {
        let mut v = ok.to_vec();
        v.retain(|(k, _)| *k != missing);
        assert!(read(&v).unwrap_err().to_string().starts_with(missing));
    }
    assert_eq!(
        read(&[
            (ENV_DIR, "/e"),
            (ENV_DATA, "/d"),
            (ENV_FOLDER, "/f"),
            (ENV_GRACE, "30")
        ])
        .unwrap()
        .unwrap()
        .grace_secs,
        30
    );
}
