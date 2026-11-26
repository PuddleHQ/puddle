// SPDX-License-Identifier: AGPL-3.0-or-later
//! A backup restored by hand into a new data folder on a new machine, as `docs/backup.md` says,
//! then a started host: what the backup holds is there, and what it leaves out is asked for again.

use super::*;

/// What a user can read from a started host about the parts a backup carries.
#[derive(Debug, PartialEq)]
struct Seen {
    rules: serde_json::Value,
    settings: serde_json::Value,
    workspace_settings: serde_json::Value,
    identities: Vec<(String, serde_json::Value)>,
    workspaces: Vec<(String, String)>,
    global_env: serde_json::Value,
    workspace_env: serde_json::Value,
    audit: Vec<serde_json::Value>,
}

impl Seen {
    async fn of(api: &Api) -> Self {
        let identities = api.get("/api/identities").await.json()["identities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| (i["label"].as_str().unwrap().to_owned(), i["author"].clone()))
            .collect();
        let workspaces = api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| {
                (
                    w["id"].as_str().unwrap().to_owned(),
                    w["name"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let mut audit = api.get("/api/audit?limit=500&after=0").await.json()["entries"]
            .as_array()
            .unwrap()
            .clone();
        audit.sort_by_key(|e| e["id"].as_i64().unwrap());
        Self {
            rules: api.get("/api/rules").await.json()["rules"].clone(),
            settings: api.get("/api/settings").await.json(),
            workspace_settings: api.get("/api/settings/workspaces/acme").await.json(),
            identities,
            workspaces,
            global_env: api.get("/api/env").await.json()["variables"].clone(),
            workspace_env: api.get("/api/workspaces/acme/env").await.json()["variables"].clone(),
            audit,
        }
    }

    /// The parts that differ, by name. The restored host may have written audit records of its
    /// own after the backed-up ones, so the audit counts as equal when it starts with them.
    fn differences(&self, restored: &Self) -> Vec<&'static str> {
        let mut found = Vec::new();
        let mut check = |name: &'static str, equal: bool| {
            if !equal {
                found.push(name);
            }
        };
        check("rules", self.rules == restored.rules);
        check("settings", self.settings == restored.settings);
        check(
            "workspace settings",
            self.workspace_settings == restored.workspace_settings,
        );
        check("identities", self.identities == restored.identities);
        check("workspace list", self.workspaces == restored.workspaces);
        check("global variables", self.global_env == restored.global_env);
        check(
            "workspace variables",
            self.workspace_env == restored.workspace_env,
        );
        check("audit", restored.audit.starts_with(&self.audit));
        found
    }
}

/// A machine with every kind of data a backup carries, stopped, and what it showed.
async fn populated_machine() -> (Rig, Seen) {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let rule =
        json!({"scope": {"type": "global"}, "pattern": "api.example.org", "effect": "allow"});
    assert_eq!(api.post("/api/rules", &rule.to_string()).await.status, 201);
    put_env(
        &api,
        "/api/settings",
        json!({"ui": {"disk_warning_percent": 99}}),
    )
    .await;
    create(&api, &mut events, "acme").await;
    put_env(
        &api,
        "/api/settings/workspaces/acme",
        json!({"overrides": {"reconnection_grace": 90}}),
    )
    .await;
    put_env(
        &api,
        "/api/env/GLOBAL_TOKEN",
        secret_body(Some("global-value"), &["api.example.org"]),
    )
    .await;
    put_env(
        &api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some("workspace-value"), &["keys.example.org"]),
    )
    .await;
    add_github_identity(&api, "ghp_stored_on_the_old_machine").await;
    let seen = Seen::of(&api).await;
    assert!(
        !seen.audit.is_empty(),
        "the audit is not empty to begin with"
    );
    assert_eq!(seen.identities.len(), 1);
    assert_eq!(seen.workspaces.len(), 1);
    host.shutdown().await;
    (rig, seen)
}

/// Copies the folder `from` to `to`, files and folders alike.
fn copy_folder(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_folder(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// `docs/backup.md`, "Restore": the parts of a backup copied into the new data folder by hand.
/// `skipping` leaves one of them out, the breakage a test of this must catch.
fn restore_by_hand(backup: &std::path::Path, target: &Rig, skipping: Option<&str>) {
    let data = target.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    for part in ["puddle.db", "workspaces.json"] {
        if skipping != Some(part) {
            std::fs::copy(backup.join(part), data.join(part)).unwrap();
        }
    }
    if skipping != Some("settings") {
        copy_folder(&backup.join("settings"), &data.join("settings"));
    }
}

/// The backup `puddle backup` makes of a stopped machine.
fn back_up(machine: &Rig) -> PathBuf {
    let backup = machine.dir.path().join("backup");
    puddle_host::backup(&HostPaths::new(machine.dir.path().join("data")), &backup).unwrap();
    backup
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_copied_into_a_new_data_folder_starts_with_its_data_and_asks_for_what_it_left_out()
{
    let (old_machine, before) = populated_machine().await;
    // A new machine: its own data folder, no sandbox disks, an empty credential store.
    let new_machine = Rig::named_ids();
    restore_by_hand(&back_up(&old_machine), &new_machine, None);
    let host = new_machine.start().await;
    let api = api(&host);

    let after = Seen::of(&api).await;
    assert_eq!(before.differences(&after), Vec::<&str>::new());
    assert!(
        after.rules.to_string().contains("api.example.org"),
        "{}",
        after.rules
    );
    assert_eq!(after.settings["ui"]["disk_warning_percent"], 99);

    // The workspace is listed and its disk is not here.
    let shown = api.get("/api/workspaces/acme").await.json();
    assert_eq!(shown["status"], "volume_missing", "{shown}");

    // The identity's token was never in the backup: its credential reads as needing a sign-in.
    let identity = api.get("/api/identities").await.json();
    let source = identity["identities"][0]["credentials"][0]["source"].clone();
    let check = api
        .post(
            "/api/credentials/check",
            &json!({"source": source}).to_string(),
        )
        .await;
    assert_eq!(check.status, 200, "{}", check.body);
    let check = check.json();
    assert_eq!(check["readable"], false, "{check}");
    assert_eq!(check["needs_sign_in"], true, "{check}");

    // The secrets arrive named, with no value: the next start that needs one says which to set
    // again, and the store of this machine holds none.
    for (list, name) in [
        (&after.global_env, "GLOBAL_TOKEN"),
        (&after.workspace_env, "API_KEY"),
    ] {
        let found = list.as_array().unwrap().iter().find(|v| v["name"] == name);
        assert_eq!(found.map(|v| &v["kind"]), Some(&json!("secret")), "{list}");
    }
    assert!(new_machine.secrets.ids().is_empty());
    let mut events = api.events().await;
    let reply = api.post("/api/workspaces", &new_workspace("fresh")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("fresh"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(
        detail.contains("GLOBAL_TOKEN") && detail.contains("Settings for a global secret"),
        "{detail}"
    );
    assert!(!detail.contains("global-value"), "{detail}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_that_skips_a_part_of_the_backup_is_seen_as_a_difference() {
    let (old_machine, before) = populated_machine().await;
    let backup = back_up(&old_machine);
    for (skipped, shown_as) in [
        ("settings", ["settings", "workspace settings"].as_slice()),
        ("workspaces.json", ["workspace list"].as_slice()),
    ] {
        let new_machine = Rig::named_ids();
        restore_by_hand(&backup, &new_machine, Some(skipped));
        let host = new_machine.start().await;
        let after = Seen::of(&api(&host)).await;
        host.shutdown().await;
        let found = before.differences(&after);
        for name in shown_as {
            assert!(found.contains(name), "skipping {skipped}: {found:?}");
        }
    }
}
