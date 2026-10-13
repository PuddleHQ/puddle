// SPDX-License-Identifier: GPL-3.0-or-later
//! A sandbox whose stored configuration cannot be read is refused, never read as "no volumes".
//!
//! The configuration lives in msb's database. Both tests damage it there, with the VM up or
//! stopped as the case needs: `start` refuses the damaged sandbox, and a second sandbox cannot
//! attach a volume while the damaged one is running, since who holds the volume is unknown. Each
//! error names the sandbox and the way out, and nothing is created or started by the refusal.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "VM test helpers: a failed step fails the test"
)]

mod support;

use std::path::Path;
use std::time::Duration;

use puddle_compute::{DiskSize, Runtime, Sandbox, SandboxSpec, VolumeMount};
use puddle_compute_msb::MsbRuntime;
use puddle_types::{GuestPath, ImageRef, MemoryMib, SandboxName, VolumeName};
use puddle_vm_tests::{DEBIAN_DEVCONTAINER, Settings};

const DAMAGED: &str = "{ this is not a sandbox configuration";

fn name(settings: &Settings, tag: &str) -> SandboxName {
    SandboxName::new(&format!("{}-{tag}", settings.prefix)).unwrap()
}

fn spec(settings: &Settings, tag: &str) -> SandboxSpec {
    SandboxSpec::new(
        name(settings, tag),
        ImageRef::new(DEBIAN_DEVCONTAINER).unwrap(),
    )
    .with_memory(MemoryMib::new(512).unwrap())
}

fn with_volume(spec: SandboxSpec, volume: &VolumeName) -> SandboxSpec {
    spec.with_volume(
        VolumeMount::named(volume.clone(), GuestPath::new("/workspaces/ws").unwrap())
            .ensure_size(DiskSize::mib(256)),
    )
}

/// Sets the stored configuration of `sandbox` in the msb database under `home` and returns what
/// was there. msb keeps the document as JSON text in the `sandbox` table's `config` column.
fn set_stored_config(home: &Path, sandbox: &SandboxName, json: &str) -> String {
    let db = rusqlite::Connection::open(home.join("db").join("msb.db")).expect("open msb database");
    db.busy_timeout(Duration::from_secs(10)).unwrap();
    let before: String = db
        .query_row(
            "SELECT config FROM sandbox WHERE name = ?1",
            [sandbox.as_str()],
            |row| row.get(0),
        )
        .expect("the sandbox's row");
    let changed = db
        .execute(
            "UPDATE sandbox SET config = ?1 WHERE name = ?2",
            [json, sandbox.as_str()],
        )
        .unwrap();
    assert_eq!(changed, 1, "exactly one sandbox row is damaged");
    before
}

async fn cleanup(rt: &MsbRuntime, names: &[&SandboxName]) {
    for n in names {
        if let Ok(sb) = rt.get(n).await {
            let _ = sb.stop().await;
        }
        let _ = rt.remove(n).await;
    }
}

/// A stopped sandbox with a damaged stored configuration cannot start: the refusal names it and
/// the way out, and it stays stopped. With the configuration back, it starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_damaged_stored_config_refuses_start() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "damaged-start");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    sb.stop().await.expect("stop");

    let good = set_stored_config(&settings.home(), &n, DAMAGED);
    let refused = rt.start(&n).await.err().map(|e| e.to_string());
    let still_down = rt.get(&n).await.is_err();
    set_stored_config(&settings.home(), &n, &good);
    let restarted = rt.start(&n).await.is_ok();
    cleanup(&rt, &[&n]).await;

    let message = refused.expect("start refuses a damaged stored configuration");
    assert!(message.contains(n.as_str()), "names the sandbox: {message}");
    assert!(
        message.contains("cannot be read") && message.contains("remove the sandbox"),
        "says what is wrong and the way out: {message}"
    );
    assert!(still_down, "the refused start left the sandbox stopped");
    assert!(restarted, "with its configuration back it starts");
}

/// While a running sandbox's stored configuration is damaged, a second sandbox cannot attach a
/// volume: the first one may hold it. The refusal names the damaged sandbox, and the second
/// sandbox is not created. With the configuration back, the volume is in use by the first and
/// the second is refused for that, as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_damaged_stored_config_refuses_a_second_attach() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let volume = VolumeName::new(&format!("{}-damaged", settings.prefix)).unwrap();
    let first = with_volume(spec(&settings, "damaged-first"), &volume);
    let second = with_volume(spec(&settings, "damaged-second"), &volume);
    let (n1, n2) = (first.name.clone(), second.name.clone());
    let _running = rt.create(first).await.expect("create the first");

    let good = set_stored_config(&settings.home(), &n1, DAMAGED);
    let refused = rt.create(second.clone()).await.err().map(|e| e.to_string());
    let created = rt.get(&n2).await.is_ok();
    set_stored_config(&settings.home(), &n1, &good);
    let in_use = rt.create(second).await.err().map(|e| e.to_string());
    cleanup(&rt, &[&n2, &n1]).await;
    let _ = rt.remove_volume(&volume).await;

    let message = refused.expect("a second attach is refused while the holder is unknown");
    assert!(
        message.contains(n1.as_str()),
        "names the damaged sandbox: {message}"
    );
    assert!(
        message.contains("cannot be read") && message.contains("remove the sandbox"),
        "says what is wrong and the way out: {message}"
    );
    assert!(!created, "the refused attach created nothing");
    let in_use = in_use.expect("the volume is still held by the first sandbox");
    assert!(
        !in_use.contains("cannot be read") && in_use.contains(n1.as_str()),
        "with a readable configuration the refusal names the holder: {in_use}"
    );
}
