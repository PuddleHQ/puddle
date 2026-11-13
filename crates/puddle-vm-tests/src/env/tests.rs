// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_runtime::HostOs;
use std::collections::HashMap;

use super::*;

#[test]
fn a_registry_is_the_host_part_of_the_reference_or_docker_hub() {
    assert_eq!(registry_of("node:22-bookworm"), "registry-1.docker.io");
    assert_eq!(registry_of("denoland/deno:debian"), "registry-1.docker.io");
    assert_eq!(
        registry_of("mcr.microsoft.com/dotnet/sdk:10.0"),
        "mcr.microsoft.com"
    );
    assert_eq!(registry_of("localhost:5000/x"), "localhost:5000");
}

#[tokio::test]
async fn a_failed_creation_names_the_registry_and_the_cause() {
    let failed = create_from_registry::<(), _>("node:22", Duration::from_secs(5), async {
        Err("manifest unknown")
    })
    .await
    .unwrap_err();
    let text = failed.to_string();
    assert!(text.contains("registry-1.docker.io"), "{text}");
    assert!(text.contains("manifest unknown"), "{text}");
    let ok = create_from_registry("node:22", Duration::from_secs(5), async {
        Ok::<_, String>(7)
    })
    .await;
    assert_eq!(ok.unwrap(), 7);
}

#[tokio::test]
async fn a_creation_that_never_answers_names_the_registry_and_the_budget() {
    let hung = create_from_registry::<(), String>(
        "mcr.microsoft.com/dotnet/sdk:10.0",
        Duration::from_millis(50),
        std::future::pending(),
    )
    .await
    .unwrap_err();
    let text = hung.to_string();
    assert!(text.contains("mcr.microsoft.com"), "{text}");
    assert!(text.contains("50ms"), "{text}");
}

#[test]
fn the_metrics_segment_name_matches_msbs_hash_of_the_home() {
    // Value from msb's own `metrics_registry_shm_name` for this path.
    assert_eq!(
        // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
        metrics_segment_names(Path::new("/tmp/msb-abc123")),
        ["msb-met-d63328847086d141-v2", "msb-met-d63328847086d141-v3"]
    );
}

/// A temp dir removed on drop, named after `tag`.
pub(crate) struct TempDir(tempfile::TempDir);

impl TempDir {
    /// Owner-only whatever the umask, because msb refuses a home under a folder other
    /// accounts can write to.
    pub(crate) fn new(tag: &str) -> Self {
        let prefix = format!("puddle-vm-tests-{tag}-");
        let mut builder = tempfile::Builder::new();
        builder.prefix(&prefix);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        Self(builder.tempdir().unwrap())
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }
}

fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    move |var| map.get(var).cloned()
}

#[test]
fn settings_read_runtime_prefix_and_root() {
    let settings = Settings::from_lookup(lookup(&[
        (RUNTIME_DIR_VAR, "/rt"),
        (PREFIX_VAR, "r9-1"),
        (ROOT_VAR, "/vm"),
    ]))
    .unwrap();
    assert_eq!(settings.runtime_dir, PathBuf::from("/rt"));
    assert_eq!(settings.prefix.as_str(), "r9-1");
    assert_eq!(settings.home(), PathBuf::from("/vm").join("r9-1"));
}

#[test]
fn settings_read_the_msb_log_level_and_refuse_unknown_ones() {
    let base = [(RUNTIME_DIR_VAR, "/rt"), (PREFIX_VAR, "r9-1")];
    let unset = Settings::from_lookup(lookup(&base)).unwrap();
    assert_eq!(unset.msb_log_level, None);
    let empty = Settings::from_lookup(lookup(&[base[0], base[1], (MSB_LOG_LEVEL_VAR, "")]));
    assert_eq!(empty.unwrap().msb_log_level, None);
    let debug = Settings::from_lookup(lookup(&[base[0], base[1], (MSB_LOG_LEVEL_VAR, "Debug")]));
    assert_eq!(debug.unwrap().msb_log_level.as_deref(), Some("debug"));
    let err = Settings::from_lookup(lookup(&[base[0], base[1], (MSB_LOG_LEVEL_VAR, "loud")]))
        .unwrap_err();
    assert!(matches!(err, HarnessError::InvalidLogLevel { value } if value == "loud"));
}

#[test]
fn kept_logs_are_a_sibling_of_the_run_home() {
    let settings = Settings::from_lookup(lookup(&[
        (RUNTIME_DIR_VAR, "/rt"),
        (PREFIX_VAR, "r9-1"),
        (ROOT_VAR, "/vm"),
    ]))
    .unwrap();
    assert_eq!(settings.kept_logs(), PathBuf::from("/vm").join("r9-1-kept"));
    assert!(!settings.kept_logs().starts_with(settings.home()));
}

#[test]
fn keep_logs_copies_the_files_and_tolerates_a_missing_dir() {
    let dir = TempDir::new("keep");
    let from = dir.path().join("sandboxes").join("sb").join("logs");
    std::fs::create_dir_all(from.join("nested")).unwrap();
    std::fs::write(from.join("runtime.log"), b"vmm").unwrap();
    std::fs::write(from.join("kernel.log"), b"").unwrap();
    let to = dir.path().join("kept").join("sb").join("logs");
    keep_logs(&from, &to);
    assert_eq!(std::fs::read(to.join("runtime.log")).unwrap(), b"vmm");
    assert!(to.join("kernel.log").is_file());
    assert!(!to.join("nested").exists());
    let none = dir.path().join("kept").join("never-booted");
    keep_logs(&dir.path().join("missing"), &none);
    assert!(!none.exists());
}

#[test]
fn a_scratch_home_is_a_short_sibling_of_the_run_home() {
    let settings = Settings::from_lookup(lookup(&[
        (RUNTIME_DIR_VAR, "/rt"),
        (PREFIX_VAR, "r9-1"),
        (ROOT_VAR, "/vm"),
    ]))
    .unwrap();
    let scratch = settings.scratch_home("pp");
    assert_eq!(scratch, PathBuf::from("/vm").join("r9-1-pp"));
    // Not below the run home, and only the tag longer: msb derives Unix socket paths from
    // the home and refuses 108 bytes or more (CI's root is already 29 bytes).
    assert!(!scratch.starts_with(settings.home()));
    assert_eq!(
        scratch.as_os_str().len(),
        settings.home().as_os_str().len() + 3
    );
}

fn settings_in(root: &Path) -> Settings {
    Settings::from_lookup(lookup(&[
        (RUNTIME_DIR_VAR, "/rt"),
        (PREFIX_VAR, "r9-1"),
        (ROOT_VAR, root.to_str().unwrap()),
    ]))
    .unwrap()
}

#[cfg(unix)]
#[test]
fn a_new_root_is_owner_only_so_msb_accepts_the_homes_under_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new("root-mode");
    let root = dir.path().join("root");
    settings_in(&root)
        .claim_scratch_home("hm", "mode_test")
        .unwrap();
    let mode = std::fs::metadata(&root).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o700, "{}", root.display());
}

#[test]
fn a_second_test_cannot_claim_a_scratch_tag_the_first_holds() {
    let dir = TempDir::new("claim");
    let settings = settings_in(&dir.path().join("root"));
    let home = settings.claim_scratch_home("hh", "first_test").unwrap();
    assert_eq!(home, settings.scratch_home("hh"));
    // The same test again (a rerun) is fine; another test, or another tag, is told apart.
    settings.claim_scratch_home("hh", "first_test").unwrap();
    settings.claim_scratch_home("h2", "second_test").unwrap();
    let err = settings
        .claim_scratch_home("hh", "second_test")
        .unwrap_err();
    assert!(
        matches!(&err, HarnessError::ScratchTagTaken { first, second, .. }
                if first == "first_test" && second == "second_test"),
        "{err}"
    );
    assert!(err.to_string().contains("\"hh\""), "{err}");
}

#[test]
fn a_claim_whose_note_is_not_a_file_says_which_path() {
    let dir = TempDir::new("claim-dir");
    let settings = settings_in(dir.path());
    std::fs::create_dir(dir.path().join("r9-1-hh.owner")).unwrap();
    let err = settings.claim_scratch_home("hh", "t").unwrap_err();
    assert!(
        matches!(&err, HarnessError::Io { path, .. } if path.ends_with("r9-1-hh.owner")),
        "{err}"
    );
}

#[test]
fn a_claim_that_cannot_make_the_root_says_which_path() {
    let dir = TempDir::new("claim-io");
    // A file where the root folder should be.
    let blocker = dir.path().join("root");
    std::fs::write(&blocker, "").unwrap();
    let err = settings_in(&blocker)
        .claim_scratch_home("hh", "t")
        .unwrap_err();
    assert!(matches!(err, HarnessError::Io { .. }), "{err}");
}

#[test]
fn the_test_owner_is_the_running_tests_name() {
    // libtest names the thread it runs the test on.
    let owner = test_owner();
    assert!(
        owner.ends_with("the_test_owner_is_the_running_tests_name"),
        "{owner}"
    );
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test checks the documented default root, which is a folder under the system temp dir"
)]
fn settings_default_the_prefix_and_root() {
    let settings = Settings::from_lookup(lookup(&[
        (RUNTIME_DIR_VAR, "/rt"),
        (PREFIX_VAR, ""),
        (ROOT_VAR, ""),
    ]))
    .unwrap();
    assert!(settings.prefix.as_str().starts_with('v'));
    assert_eq!(settings.root, std::env::temp_dir().join("pvm"));
}

#[test]
fn settings_need_a_runtime_dir() {
    let err = Settings::from_lookup(lookup(&[(RUNTIME_DIR_VAR, "")])).unwrap_err();
    assert!(matches!(
        err,
        HarnessError::MissingVar {
            var: RUNTIME_DIR_VAR,
            ..
        }
    ));
}

#[test]
fn settings_refuse_ambient_msb_vars_and_bad_prefixes() {
    let err =
        Settings::from_lookup(lookup(&[(RUNTIME_DIR_VAR, "/rt"), ("MSB_PATH", "/x")])).unwrap_err();
    assert!(matches!(
        err,
        HarnessError::AmbientMsbVar { var: "MSB_PATH" }
    ));
    let err =
        Settings::from_lookup(lookup(&[(RUNTIME_DIR_VAR, "/rt"), (PREFIX_VAR, "UP")])).unwrap_err();
    assert!(matches!(err, HarnessError::InvalidPrefix { .. }));
}

#[test]
fn prepare_writes_the_private_home_config() {
    let runtime = TempDir::new("prep-rt");
    let files = HostOs::current().runtime_files();
    std::fs::write(runtime.path().join(files.msb), b"x").unwrap();
    let library = files.libkrunfw;
    std::fs::write(runtime.path().join(library), b"x").unwrap();
    let root = TempDir::new("prep-root");
    let settings = Settings {
        runtime_dir: runtime.path().to_owned(),
        prefix: RunPrefix::new("r1-1").unwrap(),
        root: root.path().to_owned(),
        msb_log_level: None,
    };
    let pair = settings.prepare().unwrap();
    let written = std::fs::read_to_string(config_path(&settings.home())).unwrap();
    assert_eq!(written, pair.config_json(None));
    let debug = Settings {
        msb_log_level: Some("debug".into()),
        ..settings.clone()
    };
    debug.prepare().unwrap();
    let written = std::fs::read_to_string(config_path(&debug.home())).unwrap();
    assert_eq!(written, pair.config_json(Some("debug")));
    assert_eq!(pair.libkrunfw, runtime.path().join(library));
}

#[test]
fn prepare_fails_on_an_incomplete_runtime() {
    let runtime = TempDir::new("prep-empty");
    let settings = Settings {
        runtime_dir: runtime.path().to_owned(),
        prefix: RunPrefix::new("r1-2").unwrap(),
        root: runtime.path().to_owned(),
        msb_log_level: None,
    };
    assert!(matches!(
        settings.prepare().unwrap_err(),
        HarnessError::RuntimeIncomplete { .. }
    ));
}

#[test]
fn prepare_reports_an_unwritable_home() {
    let runtime = TempDir::new("prep-file");
    let files = HostOs::current().runtime_files();
    std::fs::write(runtime.path().join(files.msb), b"x").unwrap();
    let library = files.libkrunfw;
    std::fs::write(runtime.path().join(library), b"x").unwrap();
    // The root is a file, so the home under it can't be created.
    let root = runtime.path().join(library);
    let settings = Settings {
        runtime_dir: runtime.path().to_owned(),
        prefix: RunPrefix::new("r1-3").unwrap(),
        root,
        msb_log_level: None,
    };
    assert!(matches!(
        settings.prepare().unwrap_err(),
        HarnessError::Io {
            action: "create",
            ..
        }
    ));
}

/// A fake runtime pair is enough for everything short of booting: the backend opens its
/// database in the private home, lists nothing and builds sandbox specs.
fn fake_settings(tag: &str) -> (TempDir, Settings) {
    let dir = TempDir::new(tag);
    let files = HostOs::current().runtime_files();
    let (msb, library) = (files.msb, files.libkrunfw);
    std::fs::write(dir.path().join(msb), b"x").unwrap();
    std::fs::write(dir.path().join(library), b"x").unwrap();
    let settings = Settings {
        runtime_dir: dir.path().to_owned(),
        prefix: RunPrefix::new("r7-1").unwrap(),
        root: dir.path().join("root"),
        msb_log_level: None,
    };
    (dir, settings)
}

#[tokio::test]
async fn env_opens_a_private_home_without_booting() {
    let (_dir, settings) = fake_settings("env-open");
    let env = VmEnv::new(settings.clone()).await.unwrap();
    assert_eq!(env.settings(), &settings);
    assert_eq!(
        env.runtime().msb.parent(),
        Some(settings.runtime_dir.as_path())
    );
    assert!(settings.home().join("config.json").is_file());
    assert_eq!(env.sandbox_name("smoke").unwrap().as_str(), "r7-1-smoke");
    assert_eq!(
        env.log_dir("r7-1-smoke"),
        settings
            .home()
            .join("sandboxes")
            .join("r7-1-smoke")
            .join("logs")
    );
    let builder = env.sandbox("smoke", crate::DEBIAN_DEVCONTAINER);
    assert!(builder.is_ok());
    assert!(env.sandbox("Bad_Tag", crate::DEBIAN_DEVCONTAINER).is_err());
    assert_eq!(Box::pin(env.cleanup()).await.unwrap(), Vec::<String>::new());
    std::fs::create_dir_all(settings.kept_logs().join("sb").join("logs")).unwrap();
    env.remove_home().await.unwrap();
    assert!(!settings.home().exists());
    assert!(!settings.kept_logs().exists());
}

#[test]
fn a_recorded_process_id_is_used_and_a_missing_or_negative_one_is_named() {
    assert_eq!(pid_of_record(Some(4242), "box").unwrap(), 4242);
    for recorded in [None, Some(-1)] {
        let err = pid_of_record(recorded, "box").unwrap_err();
        assert!(matches!(&err, HarnessError::NoVmProcess { name } if name == "box"));
        assert!(err.to_string().contains("box"), "{err}");
    }
}

#[tokio::test]
async fn a_sandbox_msb_does_not_know_has_no_process_id() {
    let (_dir, settings) = fake_settings("env-pid");
    let env = VmEnv::new(settings).await.unwrap();
    let err = env.process_id_of("r7-1-nothing").await.unwrap_err();
    assert!(matches!(err, HarnessError::Sdk(_)), "{err}");
}

#[tokio::test]
async fn removal_accepts_a_missing_dir_and_reports_a_stuck_one() {
    let dir = TempDir::new("rm");
    let gone = dir.path().join("gone");
    remove_dir_retrying(&gone, 3, Duration::ZERO).await.unwrap();
    let full = dir.path().join("full");
    std::fs::create_dir_all(full.join("sub")).unwrap();
    std::fs::write(full.join("sub").join("f"), b"x").unwrap();
    remove_dir_retrying(&full, 3, Duration::ZERO).await.unwrap();
    assert!(!full.exists());
    // A plain file isn't a directory: every attempt fails, the last error is reported.
    let file = dir.path().join("file");
    std::fs::write(&file, b"x").unwrap();
    let err = remove_dir_retrying(&file, 3, Duration::ZERO)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            HarnessError::Io {
                action: "remove",
                ..
            }
        ),
        "{err}"
    );
}

#[tokio::test]
async fn within_passes_values_and_names_timeouts() {
    assert_eq!(
        within("quick", Duration::from_secs(5), async { 7 })
            .await
            .unwrap(),
        7
    );
    let err = within(
        "slow",
        Duration::from_millis(10),
        std::future::pending::<()>(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HarnessError::Timeout { what: "slow", .. }));
}
