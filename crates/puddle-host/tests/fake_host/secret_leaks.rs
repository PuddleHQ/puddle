// SPDX-License-Identifier: AGPL-3.0-or-later
//! Leak guards for the secret stores: three random canaries (an environment secret, a pasted login
//! token and a proxy password) go through every path that handles a secret, with logging at
//! trace, and none may appear in a log line, an event, an API answer, a backup or any file of the
//! data and config folders, the vault file included.

use super::*;

use puddle_secrets::StoreKind;

use super::terminals_and_mise::files_holding;

/// `PUDDLE-CANARY-` and 16 random hex digits.
fn random_canary() -> String {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    let mut hex = String::from("PUDDLE-CANARY-");
    for byte in bytes {
        write!(hex, "{byte:02x}").unwrap();
    }
    hex
}

/// The folder at `dest` and the archive the API streams, both as the names and bytes of what a
/// backup holds.
fn backup_names(dest: &std::path::Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut folders = vec![dest.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap().flatten() {
            if entry.path().is_dir() {
                folders.push(entry.path());
            } else {
                names.push(
                    entry
                        .path()
                        .strip_prefix(dest)
                        .unwrap()
                        .display()
                        .to_string(),
                );
            }
        }
    }
    names.sort();
    names
}

/// `puddle secrets migrate --to <to>`; the command blocks, so it runs off the async threads.
async fn migrate(
    paths: &HostPaths,
    setup: &puddle_host::SecretSetup,
    to: StoreKind,
) -> puddle_host::MigrateOutcome {
    let (paths, setup) = (paths.clone(), setup.clone());
    tokio::task::spawn_blocking(move || puddle_host::vault_migrate(&paths, &setup, to).unwrap())
        .await
        .unwrap()
}

/// Checks that the store holds every canary: the positive control, so the greps look at a store
/// that held them.
fn assert_held(store: &MemoryStore, canaries: &[&String]) {
    let held: Vec<String> = store
        .ids()
        .iter()
        .filter_map(|id| {
            store
                .get(&puddle_secrets::StoredId::new(id.as_str()).unwrap())
                .unwrap()
        })
        .map(|secret| secret.expose().to_owned())
        .collect();
    for canary in canaries {
        assert!(
            held.iter().any(|value| value.contains(canary.as_str())),
            "{canary} did not reach the operating system's store"
        );
    }
}

/// `puddle doctor --no-boot` on this rig's data folder.
fn doctor_report(rig: &Rig, data: &std::path::Path) -> puddle_doctor::Report {
    let probe = puddle_doctor::SystemProbe::new(
        rig.dir.path().join("runtime"),
        RuntimeVersion::built_for(),
        puddle_runtime::DevOverride::from_env(),
    )
    .with_data_dir(data)
    .with_sealed_key_search(rig.dir.path().join("config"), "")
    .with_az_search_path(Some(std::ffi::OsString::new()));
    puddle_doctor::diagnose(
        &probe,
        &puddle_doctor::Options {
            boot: false,
            ..puddle_doctor::Options::default()
        },
        puddle_types::VERSION,
    )
}

/// Text of what the API answers for every read that could mention a secret.
async fn api_answers(api: &Api) -> String {
    let mut text = String::new();
    for path in [
        "/api/problems",
        "/api/env",
        "/api/workspaces",
        "/api/workspaces/acme/env",
        "/api/identities",
        "/api/credentials",
        "/api/rules",
        "/api/audit",
    ] {
        let reply = api.get(path).await;
        writeln!(text, "{path} {}\n{}", reply.status, reply.body).unwrap();
    }
    text
}

#[tokio::test(flavor = "multi_thread")]
async fn no_secret_reaches_a_log_an_event_an_answer_a_backup_or_a_file_in_the_clear() {
    let logs = support::capture_logs();
    let env_secret = random_canary();
    let login_token = random_canary();
    let proxy_password = random_canary();
    let canaries = [&env_secret, &login_token, &proxy_password];
    let proxy = puddle_upstream::ProxyAddr::new("proxy.corp", 8080);
    let rig = Rig::named_ids();
    rig.file_vault.store(true, Ordering::SeqCst);
    let data = rig.dir.path().join("data");
    let mut seen = String::new();

    // A headless start: the three values go in, a workspace starts and stops with them.
    let host = rig.start().await;
    proxy_sign_ins_loaded(&host, 0).await;
    let first = api(&host);
    let mut events = first.events().await;
    put_env(
        &first,
        "/api/env/GLOBAL_TOKEN",
        secret_body(Some(&env_secret), &["api.example.org"]),
    )
    .await;
    add_github_identity(&first, &login_token).await;
    host.proxy_passwords()
        .set(&proxy, "svc", &proxy_password)
        .unwrap();
    create(&first, &mut events, "acme").await;
    first.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    first.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    seen.push_str(&api_answers(&first).await);
    seen.push_str(&serde_json::to_string(&events.seen).unwrap());
    host.shutdown().await;
    drop(host);

    // The same values move to the operating system's store and back, as `puddle secrets migrate`
    // does with the host stopped.
    let os = Arc::new(MemoryStore::new());
    let mut options = hermetic_options();
    options.secret_setup.key_home = Some(rig.dir.path().join("config"));
    options.secret_setup.tools = Some(puddle_secrets::ToolPaths::default());
    options.secret_setup.os_store = Some(os.clone());
    let setup = options.secret_setup;
    let paths = HostPaths::new(data.clone());
    let to_os = migrate(&paths, &setup, StoreKind::Os).await;
    write!(seen, "{to_os:?}").unwrap();
    // The positive control: the values did move, so the greps below look at a store that held them.
    assert_held(&os, &canaries);
    assert_eq!(
        files_holding(rig.dir.path(), &env_secret),
        Vec::<String>::new()
    );
    let to_file = migrate(&paths, &setup, StoreKind::File).await;
    write!(seen, "{to_file:?}").unwrap();
    let (status_paths, status_setup) = (paths.clone(), setup.clone());
    let status = tokio::task::spawn_blocking(move || {
        puddle_host::vault_status(&status_paths, &status_setup).unwrap()
    })
    .await
    .unwrap();
    write!(seen, "{status:?}").unwrap();

    // A restart (the start-up sweep reads every id), as a recorded install does it.
    rig.without_headless_flag.store(true, Ordering::SeqCst);
    let again = rig.start().await;
    proxy_sign_ins_loaded(&again, 1).await;
    let second = api(&again);
    seen.push_str(&api_answers(&second).await);
    assert!(problems_of(&second).await.is_empty());
    again.shutdown().await;
    drop(again);

    // A backup, as a folder and as the archive the API streams.
    let dest = rig.dir.path().join("backup");
    puddle_host::backup(&paths, &dest).unwrap();
    let names = backup_names(&dest);
    assert!(
        names
            .iter()
            .all(|n| !n.contains("secrets") && !n.contains("vault") && !n.contains("store.json")),
        "{names:?}"
    );
    assert!(puddle_host::backup_archive(&paths).is_ok());

    // The doctor, as text and as JSON.
    let report = doctor_report(&rig, &data);
    seen.push_str(&report.to_text());
    seen.push_str(&report.to_json());
    assert!(
        report.to_text().contains("file vault"),
        "the doctor looked at the vault: {}",
        report.to_text()
    );

    // Nothing holds a canary in the clear.
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    for canary in canaries {
        let canary = canary.as_str();
        assert!(!logged.contains(canary), "{canary} is in a log line");
        assert!(
            !seen.contains(canary),
            "{canary} is in an event, an answer, a report or an outcome"
        );
        assert!(!guest_has(&rig, canary), "{canary} reached the guest");
        assert_eq!(
            files_holding(rig.dir.path(), canary),
            Vec::<String>::new(),
            "{canary} is in a file (the vault holds it encrypted)"
        );
    }
    assert!(puddle_secrets::vault_path(&data).is_file());
}
