// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every kept data folder of an older Puddle opens in this build.
//!
//! `tests/fixtures/upgrade/<name>/data/` is a whole data folder as an older build left it (the
//! database, `workspaces.json`, `settings/`), `expected.json` next to it says what the user sees:
//! the workspace ids and the rule patterns. `ci/capture-upgrade-fixture.sh` adds one at each
//! release tag; the test runs them all, so a build that stops reading an old folder fails here.
//! The msb home and the files on a workspace's volume cross a real upgrade in the MSI job
//! (`msi.yml`, from every published release); the fake runtime keeps no disk to carry over.

use super::*;

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    let mut found: Vec<String> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    found.sort();
    found
}

#[tokio::test(flavor = "multi_thread")]
async fn every_kept_data_folder_of_an_older_build_opens_and_shows_what_it_held() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade");
    let mut names: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no kept data folder in {}",
        root.display()
    );
    for name in names {
        let kept = root.join(&name);
        let expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(kept.join("expected.json")).unwrap()).unwrap();
        let rig = Rig::new();
        copy_dir(&kept.join("data"), &rig.dir.path().join("data"));
        let host = rig.start().await;
        let api = api(&host);
        let listed = api.get("/api/workspaces").await.json();
        let ids: Vec<String> = listed["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, strings(&expected["workspaces"]), "{name:?}: {listed}");
        let rules = api.get("/api/rules").await;
        assert_eq!(rules.status, 200, "{name:?}: {}", rules.body);
        let patterns: Vec<String> = rules.json()["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["pattern"].as_str().unwrap().to_owned())
            .collect();
        let mut patterns = patterns;
        patterns.sort();
        assert_eq!(patterns, strings(&expected["rules"]), "{name:?}");
        host.shutdown().await;
    }
}
