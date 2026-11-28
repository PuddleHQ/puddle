// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real `guest/boot.sh`, run on the test host against a fake root with stubbed `sysctl`,
//! `update-ca-certificates` and `puddle-agent`, under every POSIX shell the host has (dash and
//! bash at least). The VM version of these checks is `tests/vm_boot.rs`.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns: a failed setup fails the test"
)]

#[path = "../common/mod.rs"]
mod common;

mod agent;
mod apply;
mod capture;
mod entrypoint;
mod hook;
mod merge;
mod session;

use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use common::{FakeRoot, alive, kill9, shells, show};
use puddle_boot::{
    BootHook, BootPlan, CA_DIR_GUEST, ENV_FILE_GUEST, GIT_CONFIG_GUEST, Gate, GateState,
    GitIdentity, MACHINE_SETTINGS_GUEST, PATH_FILE_GUEST, PLAN_HEADER, PROMPT_NAME_FILE_GUEST,
    START_FOLDER_FILE_GUEST, SUDOERS_SESSION_GUEST, machine_settings_path, prompt_name_file,
    start_folder_file,
};
use puddle_compute::ImageConfig;
use puddle_test_kit::eventually_blocking;
use puddle_types::{
    FileMode, GuestEnv, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec, SessionUser,
};

const IMAGE_PATH: &str = "/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin";

fn image(entrypoint: &[String], cmd: &[&str]) -> ImageConfig {
    ImageConfig {
        entrypoint: entrypoint.to_vec(),
        cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
        env: vec![("PATH".into(), IMAGE_PATH.into())],
        ..ImageConfig::default()
    }
}

fn env() -> GuestEnv {
    let mut env = GuestEnv::new();
    env.set("HTTPS_PROXY", "http://127.0.0.1:3128").unwrap();
    env.set("ODD", "it's $HOME `x` \\ \"q\"\nline2").unwrap();
    env
}

fn file(path: &str, contents: &[u8]) -> GuestFile {
    GuestFile::new(GuestPath::new(path).unwrap(), contents.to_vec())
}

const CA: &str = "/usr/local/share/ca-certificates/puddle/root.crt";

fn full_plan(ca: &str) -> BootPlan {
    BootPlan::builder(&image(&[], &["bash"]))
        .env(&env())
        .file(file(CA, ca.as_bytes()))
        .file(file("/etc/npmrc", b"proxy=http://127.0.0.1:3128\n").with_mode(FileMode::new(0o600)))
        .git_identity(GitIdentity::new("Ada Lovelace", "ada@example.org").unwrap())
        .build()
        .unwrap()
}

fn count(calls: &[String], prefix: &str) -> usize {
    calls.iter().filter(|c| c.starts_with(prefix)).count()
}

/// Sources `files` in a fresh shell and prints `expr`.
fn source_and_print(fr: &FakeRoot, files: &[&str], expr: &str) -> String {
    let mut parts: Vec<String> = files
        .iter()
        .map(|f| format!(". '{}';", fr.path(f).display()))
        .collect();
    parts.push(format!("printf '%s' \"{expr}\""));
    let script = parts.join(" ");
    let out = std::process::Command::new(&fr.shell)
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}
