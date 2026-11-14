// SPDX-License-Identifier: AGPL-3.0-or-later
//! The adapter's own VM bars, beyond the contract suite: ext4 volumes and owned disks,
//! three sandboxes at once with their own routes, `--max-memory` never set, the guest's open-file
//! limits, a stop that gives the guest's processes their SIGTERM first, and (fork) SSH reporting a
//! signal-killed command as a failure.
mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use microsandbox::Sandbox as SdkSandbox;
use puddle_compute::{DiskSize, FileMount, OwnedDisk, Runtime, Sandbox, VolumeMount, VsockRoute};
use puddle_ipc::IpcRoot;
use puddle_test_kit::{eventually, eventually_async};
use puddle_types::{MemoryMib, SandboxName, VolumeName};
use puddle_vm_tests::VmEnv;
use support::{cleanup, collect_lines, path, sh, spec, vsock_sweep};

/// The guest port Puddle routes.
const ROUTE_PORT: u32 = 5000;

/// The host's vsock address.
const HOST_CID: u32 = 2;

/// Named volumes and owned disks are ext4 in the guest.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_volume_and_owned_disk_are_ext4() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let vol = VolumeName::new(&format!("{}-ext4", settings.prefix)).unwrap();
    let s = spec(&settings, "ext4")
        .with_volume(
            VolumeMount::named(vol.clone(), path("/workspaces/ws")).ensure_size(DiskSize::mib(256)),
        )
        .with_owned_disk(OwnedDisk {
            guest: path("/var/lib/docker"),
            size: DiskSize::mib(256),
        });
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let out = sh(
        &sb,
        r#"awk '$2 == "/workspaces/ws" || $2 == "/var/lib/docker" { print $2, $3 }' /proc/mounts | sort"#,
    )
    .await;
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    let _ = rt.remove_volume(&vol).await;
    assert_eq!(
        out.stdout_text(),
        "/var/lib/docker ext4\n/workspaces/ws ext4\n"
    );
}

/// Three sandboxes at once, each with its own route and file mount: every one runs commands,
/// keeps its mount read-only, and what it sends arrives on its own endpoint only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_three_sandboxes_at_once_each_with_its_own_route() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let root = IpcRoot::new().unwrap();
    let share = rt.config().guest_share.join("three");
    std::fs::create_dir_all(&share).unwrap();
    let mut specs = Vec::new();
    let mut collectors = Vec::new();
    for i in 0..3 {
        let listener = root.listen().unwrap();
        let file = share.join(format!("id-{i}"));
        std::fs::write(&file, format!("sandbox-{i}\n")).unwrap();
        let s = spec(&settings, &format!("three-{i}"))
            .with_route(VsockRoute::new(ROUTE_PORT, listener.endpoint().path()))
            .with_file_mount(FileMount::read_only(&file, path("/puddle/id")));
        collectors.push(collect_lines(listener));
        specs.push(s);
    }
    let names: Vec<SandboxName> = specs.iter().map(|s| s.name.clone()).collect();
    let mut specs = specs.into_iter();
    let (a, b, c) = tokio::join!(
        rt.create(specs.next().unwrap()),
        rt.create(specs.next().unwrap()),
        rt.create(specs.next().unwrap()),
    );
    let sandboxes = [
        a.expect("create 0"),
        b.expect("create 1"),
        c.expect("create 2"),
    ];
    let mut failures = Vec::new();
    for (i, sb) in sandboxes.iter().enumerate() {
        let id = sh(sb, "cat /puddle/id").await.stdout_text().into_owned();
        if id != format!("sandbox-{i}\n") {
            failures.push(format!("{i}: mount reads {id:?}"));
        }
        if sh(sb, "exit 3").await.status.code != 3 {
            failures.push(format!("{i}: exit code lost"));
        }
        let write = sh(sb, "echo x > /puddle/id").await;
        if !write.stderr_text().contains("Read-only file system") {
            failures.push(format!("{i}: mount writable: {}", write.stderr_text()));
        }
        let sweep = vsock_sweep(sb, HOST_CID, &format!("from-{i}"), &["5000"]).await;
        if sweep.accepted != [ROUTE_PORT] {
            failures.push(format!("{i}: route refused"));
        }
    }
    for sb in &sandboxes {
        sb.stop().await.unwrap();
    }
    cleanup(&rt, &names.iter().collect::<Vec<_>>()).await;
    for (i, collector) in collectors.into_iter().enumerate() {
        let lines = collector.await.unwrap();
        if lines != [format!("from-{i}")] {
            failures.push(format!("{i}: endpoint got {lines:?}"));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The created sandbox has `--memory` from the spec and no hotplug reserve (msb stores
/// max-memory = memory when it isn't set); a memory change, up or down, is persisted for the
/// next start and keeps max-memory equal to memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_memory_is_set_and_max_memory_never() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let harness = VmEnv::new(settings.clone()).await.unwrap();
    let s = spec(&settings, "mem");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let harness = &harness;
    let resources = |name: String| async move {
        let handle = harness
            .scope(Box::pin(SdkSandbox::get(&name)))
            .await
            .unwrap();
        let config = handle.config().unwrap();
        (
            config.spec.resources.memory_mib,
            config.spec.resources.max_memory_mib,
        )
    };
    let created = resources(n.to_string()).await;
    rt.set_memory(&n, MemoryMib::new(768).unwrap())
        .await
        .unwrap();
    let raised = resources(n.to_string()).await;
    sb.stop().await.unwrap();
    rt.set_memory(&n, MemoryMib::new(384).unwrap())
        .await
        .unwrap();
    let lowered = resources(n.to_string()).await;
    cleanup(&rt, &[&n]).await;
    assert_eq!(created, (512, 512), "(memory, max memory) after create");
    assert_eq!(raised, (768, 768), "(memory, max memory) after raising it");
    assert_eq!(
        lowered,
        (384, 384),
        "(memory, max memory) after lowering it"
    );
}

/// A shell in the guest, as root and as an unprivileged user, starts with the open-file limits
/// the adapter sets (soft 65 536, hard 1 048 576), and a process may raise its soft limit to
/// the hard one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_guest_shell_starts_with_the_open_file_limits() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "nofile");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let root = sh(&sb, "echo \"$(ulimit -Sn) $(ulimit -Hn)\"").await;
    let user = sh(
        &sb,
        "setpriv --reuid=65534 --regid=65534 --clear-groups sh -c 'echo \"$(ulimit -Sn) $(ulimit -Hn)\"'",
    )
    .await;
    let raised = sh(&sb, "ulimit -Sn 1048576 && ulimit -Sn").await;
    eprintln!("root: {}user: {}", root.stdout_text(), user.stdout_text());
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    assert_eq!(
        root.stdout_text().trim(),
        "65536 1048576",
        "{}",
        root.stderr_text()
    );
    assert_eq!(
        user.stdout_text().trim(),
        "65536 1048576",
        "{}",
        user.stderr_text()
    );
    assert_eq!(
        raised.stdout_text().trim(),
        "1048576",
        "{}",
        raised.stderr_text()
    );
}

/// A mount source outside the guest-share root is refused before anything is created.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_mount_sources_outside_the_share_are_refused() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let outside = settings.home().join("config.json");
    assert!(Path::new(&outside).is_file());
    let s = spec(&settings, "c7").with_file_mount(FileMount::read_only(&outside, path("/x")));
    let n = s.name.clone();
    let err = rt.create(s).await.unwrap_err();
    assert!(
        err.to_string().contains("outside the guest-share root"),
        "{err}"
    );
    assert!(
        rt.list()
            .await
            .unwrap()
            .iter()
            .all(|i| i.name != n.as_str())
    );
}

/// msb's SSH server (SDK, in Puddle's process) reports a signal-killed command as a failure: the
/// guest agent gives no signal number, so the server sends exit status 1, and a normal exit code
/// still arrives as is (upstream `superradcompany/microsandbox#2011`, carried by the fork).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_ssh_reports_a_signal_killed_command_as_a_failure() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let harness = VmEnv::new(settings.clone()).await.unwrap();
    let s = spec(&settings, "sshsig");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let statuses = harness
        .scope(Box::pin(async {
            let live = SdkSandbox::get(n.as_str()).await?.connect().await?;
            let client = live.ssh().open_client().await?;
            let killed = client.exec("kill -9 $$").await?.status;
            let seven = client.exec("exit 7").await?.status;
            client.close().await?;
            Ok::<_, microsandbox::MicrosandboxError>((killed, seven))
        }))
        .await;
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    let (killed, seven) = statuses.expect("SSH exec through the SDK client");
    assert_eq!(killed, 1, "a signal-killed command must report a failure");
    assert_eq!(seven, 7, "a normal exit code is reported as is");
}

/// A stop the VM never answers ends in a kill: the stop returns `Ok`, the sandbox is down, and the
/// handle says it was by force. The VM's process is frozen (`SIGSTOP`), so it cannot take part in
/// a graceful shutdown (msb gives up on the handshake with it after about 10 s, well inside the
/// 30 s stop timeout); `SIGKILL` still ends it. Unix only: a Windows process cannot be frozen from
/// a test without more than this test is worth.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_stop_the_vm_never_answers_ends_by_force() {
    use rustix::process::{Pid, Signal, kill_process};

    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let harness = VmEnv::new(settings.clone()).await.unwrap();
    let s = spec(&settings, "force");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let pid = harness
        .scope(Box::pin(async {
            SdkSandbox::get(n.as_str())
                .await
                .map(|h| h.local().and_then(|l| l.pid))
        }))
        .await
        .expect("the sandbox's record")
        .expect("the record names the VM process");
    let pid = Pid::from_raw(pid).expect("a process id");
    kill_process(pid, Signal::STOP).expect("freeze the VM process");

    let stopped = sb.stop().await;
    let forced = sb.stopped_by_force();
    let status = sb.status().await;
    // Leave nothing frozen behind if the stop did not end it.
    let _ = kill_process(pid, Signal::KILL);
    cleanup(&rt, &[&n]).await;
    stopped.expect("a forced stop still returns Ok");
    assert!(forced, "the stop ended the VM by force but said it did not");
    assert!(status.unwrap().is_down());
}

/// The other side of the bar above: a stop the guest answers is not by force.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_stop_the_guest_answers_is_not_by_force() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "clean");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let stopped = sb.stop().await;
    let forced = sb.stopped_by_force();
    cleanup(&rt, &[&n]).await;
    stopped.unwrap();
    assert!(!forced);
}

/// A stop gives every process of the guest its SIGTERM and time to finish before the VM goes down,
/// also a daemon that has left the command that started it (an image's entrypoint such as dockerd,
/// which loses its network state when it is killed instead). The daemon here writes a file when
/// it gets the SIGTERM; the file is there at the next boot. The test stops the sandbox only once
/// the daemon says its handler is installed: a SIGTERM before that kills the shell with no marker,
/// which is the daemon starting late, not a stop that skipped it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_stop_lets_a_detached_daemon_shut_down_cleanly() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "term");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let daemon = "setsid sh -c 'trap \"echo clean >/root/daemon-stop; sync; exit 0\" TERM; \
                  echo up >/root/daemon-ready; while :; do sleep 1; done' </dev/null >/dev/null 2>&1 &";
    assert!(sh(&sb, daemon).await.status.success());
    eventually_async(
        "the daemon has installed its SIGTERM handler",
        Duration::from_secs(60),
        || async {
            let out = sh(&sb, "cat /root/daemon-ready").await;
            (out.stdout_text().trim() == "up").then_some(())
        },
    )
    .await;
    let stopped = sb.stop().await;
    let forced = sb.stopped_by_force();
    let again = rt.start(&n).await.expect("start again");
    let seen = sh(&again, "cat /root/daemon-stop").await;
    drop(again.stop().await);
    cleanup(&rt, &[&n]).await;
    stopped.unwrap();
    assert!(!forced);
    assert_eq!(seen.stdout_text().trim(), "clean", "{}", seen.stderr_text());
}

/// The other side: a process that ignores SIGTERM holds the stop only for the grace period, and
/// the stop still ends in a clean shutdown (not by force).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_process_that_ignores_sigterm_does_not_hold_the_stop() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "noterm");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let stubborn =
        "setsid sh -c 'trap \"\" TERM; while :; do sleep 1; done' </dev/null >/dev/null 2>&1 &";
    assert!(sh(&sb, stubborn).await.status.success());
    let stopped = sb.stop().await;
    let forced = sb.stopped_by_force();
    let status = sb.status().await;
    cleanup(&rt, &[&n]).await;
    stopped.unwrap();
    assert!(!forced);
    assert!(status.unwrap().is_down());
}

/// What the guest says a mounted volume holds, in MiB: `df`'s used column (the file system's own
/// count, metadata included) and `du` over the files.
#[expect(
    clippy::unwrap_used,
    reason = "VM test helper: a failed step fails the test"
)]
async fn guest_use_mib<S: Sandbox>(sb: &S, mount: &str) -> (u64, u64) {
    let out = sh(
        sb,
        &format!(
            "sync; echo $(df -Pm {mount} | awk 'NR==2 {{print $3}}') $(du -sm {mount} | cut -f1)"
        ),
    )
    .await;
    let text = out.stdout_text();
    let mut words = text.split_whitespace().map(|w| w.parse::<u64>().unwrap());
    (words.next().unwrap(), words.next().unwrap())
}

/// The volume's `used` as the runtime reports it, in MiB.
#[expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "VM test helper: a failed step fails the test"
)]
async fn volume_used_mib(rt: &puddle_compute_msb::MsbRuntime, vol: &VolumeName) -> u64 {
    u64::from(
        rt.volume(vol)
            .await
            .unwrap()
            .expect("the volume exists")
            .used
            .expect("the runtime reads the volume's image")
            .as_mib(),
    )
}

/// The `used` of a volume tracks what the guest holds in it: the runtime reads the allocated size
/// of the volume's disk image, so after the guest writes (and syncs) a known amount, the reading
/// grows by that amount and sits a fixed offset above the guest's own `df` (the file system's used
/// count; the offset is the journal and tables plus, on Linux, the zeros the runtime filled ahead of
/// the guest's writes, [`OFFSET_MAX_MIB`] at most). The Windows leg reads the image with
/// `GetCompressedFileSizeW`, the Linux leg with the block count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_volume_used_matches_what_the_guest_holds() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let vol = VolumeName::new(&format!("{}-used", settings.prefix)).unwrap();
    let mount = "/workspaces/ws";
    let s = spec(&settings, "used")
        .with_volume(VolumeMount::named(vol.clone(), path(mount)).ensure_size(DiskSize::mib(1024)));
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");

    let (df0, du0) = guest_use_mib(&sb, mount).await;
    let used0 = volume_used_mib(&rt, &vol).await;
    eprintln!("USED empty: runtime={used0} MiB guest df={df0} MiB du={du0} MiB");

    let mut rows = vec![(0, used0, df0)];
    for written in [128_u64, 512] {
        sh(
            &sb,
            &format!("dd if=/dev/urandom of={mount}/fill-{written} bs=1M count={written} status=none && sync"),
        )
        .await;
        let (df, du) = guest_use_mib(&sb, mount).await;
        let used = volume_used_mib(&rt, &vol).await;
        eprintln!(
            "USED after {written} MiB more: runtime={used} MiB guest df={df} MiB du={du} MiB"
        );
        rows.push((written, used, df));
    }
    sb.stop().await.unwrap();
    let stopped = volume_used_mib(&rt, &vol).await;
    eprintln!("USED stopped: runtime={stopped} MiB");
    cleanup(&rt, &[&n]).await;
    let _ = rt.remove_volume(&vol).await;

    // The image holds the file system's own structures (the journal, the tables) before the guest
    // writes a byte: a constant offset above `df`, which counts them too. What the guest writes
    // then shows up one for one.
    let (_, base, base_df) = rows[0];
    for &(written, used, df) in &rows {
        assert!(
            (df..=df + OFFSET_MAX_MIB).contains(&used),
            "after {written} MiB written the runtime says {used} MiB, the guest's df {df} MiB (the image may hold up to {OFFSET_MAX_MIB} MiB more)"
        );
        assert!(
            used.abs_diff(base + (df - base_df)) <= GROWTH_BOUND_MIB,
            "after {written} MiB written the runtime says {used} MiB, which is not {base} + {} MiB the guest holds more (bound {GROWTH_BOUND_MIB})",
            df - base_df
        );
    }
}

/// How far above the guest's `df` the image's allocation may sit, in MiB: the file system's own
/// structures plus the grow-ahead cap.
const OFFSET_MAX_MIB: u64 = FS_STRUCTURES_MAX_MIB + GROW_AHEAD_CAP_MIB;

/// The file system's own structures on a 1 GiB volume, in MiB: measured 65 MiB at every step on
/// both Linux and Windows without grow-ahead; the headroom covers a larger journal.
const FS_STRUCTURES_MAX_MIB: u64 = 128;

/// The most zeros the Linux runtime keeps filled ahead of the guest's writes in one image, in MiB
/// (its grow-ahead cap). The first write into a 16 MiB chunk with holes fills the whole chunk, so
/// the chunks the file system's structures touch read up to this much above them: measured 128 MiB
/// above `df` at every step, against 65 MiB without grow-ahead. Windows does not fill ahead.
const GROW_AHEAD_CAP_MIB: u64 = 256;

/// How far the runtime's growth may be from the guest's growth, in MiB: measured 0 at 128 and 512
/// without grow-ahead, 3 at both with it.
const GROWTH_BOUND_MIB: u64 = 16;

/// The mount points msb's bootstrap root holds (the folder it shares with the guest while the
/// image's root is assembled).
const BOOTSTRAP_ROOT_ENTRIES: [&str; 5] = [".msb", "dev", "newroot", "proc", "sys"];

/// The bootstrap roots found directly in `parent`: the folders that hold exactly the mount points
/// above.
fn bootstrap_roots_in(parent: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|dir| {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .map(|it| {
                    it.filter_map(Result::ok)
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            names.sort();
            names == BOOTSTRAP_ROOT_ENTRIES
        })
        .collect()
}

/// msb's bootstrap root (an OCI root it shares with the guest) lives below the sandbox's own
/// folder and goes with the sandbox: while the sandbox runs, the one root is
/// `<home>/sandboxes/<name>/.bootstrap-rootfs/<id>`, and nothing of the kind sits in the system's
/// temp folder (where msb used to put it, shared by every sandbox of the host). After the stop the
/// root is gone, and removing the sandbox removes its folder. On Windows msb keeps this root in
/// memory (`AgentBootstrapFs`), so there is no folder at all: the same run shows that no root
/// appears below the sandbox folder or in the temp folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_bootstrap_root_lives_in_the_sandbox_folder_and_goes_with_it() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "bsroot");
    let n = s.name.clone();
    let folder = settings.home().join("sandboxes").join(n.as_str());
    let holder = folder.join(".bootstrap-rootfs");
    #[expect(
        clippy::disallowed_methods,
        reason = "looks at the folder msb's runner uses for its temp files, not one of ours"
    )]
    let temp = std::env::temp_dir();
    // Roots older runs left in the temp folder do not count: only the ones this sandbox adds.
    let temp_before = bootstrap_roots_in(&temp);
    let sb = rt.create(s).await.expect("create");

    let running_in_folder = bootstrap_roots_in(&holder);
    let holder_exists = holder.exists();
    let added_in_temp: Vec<PathBuf> = bootstrap_roots_in(&temp)
        .into_iter()
        .filter(|dir| !temp_before.contains(dir))
        .collect();
    let guest_ok = sh(&sb, "echo up").await.status.success();
    sb.stop().await.unwrap();
    eventually(
        "the bootstrap root is removed after the stop",
        Duration::from_secs(30),
        || {
            let left = bootstrap_roots_in(&holder);
            left.is_empty().then_some(left)
        },
    )
    .await;
    let folder_kept_by_stop = folder.is_dir();
    rt.remove(&n).await.expect("remove");
    let folder_after_remove = folder.exists();
    cleanup(&rt, &[&n]).await;

    assert!(guest_ok, "the guest runs on that root");
    if cfg!(unix) {
        assert_eq!(
            running_in_folder.len(),
            1,
            "one bootstrap root below {holder:?} while the sandbox runs"
        );
    } else {
        assert!(
            !holder_exists && running_in_folder.is_empty(),
            "Windows keeps the bootstrap root in memory: no {holder:?} folder, found {running_in_folder:?}"
        );
    }
    assert!(
        added_in_temp.is_empty(),
        "the sandbox put bootstrap roots in the temp folder: {added_in_temp:?}"
    );
    assert!(folder_kept_by_stop, "a stop keeps the sandbox folder");
    assert!(
        !folder_after_remove,
        "removing the sandbox removes its folder with the root's parent"
    );
}

/// A grow of a stopped root leaves the old image behind (msb copies the root into a `.grow-*`
/// folder and never deletes the original); the next start deletes what the journal no longer
/// names, and the grown disk, its files and the live grow on top of it all still work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_a_stopped_grow_leaves_no_orphaned_root_image_after_the_next_start() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let harness = VmEnv::new(settings.clone()).await.unwrap();
    let s = spec(&settings, "orphan");
    let n = s.name.clone();
    let dir = settings.home().join("sandboxes").join(n.as_str());
    let sb = rt.create(s).await.expect("create");
    let wrote = sh(&sb, "echo orphan-seed > /root/seed; sync").await;
    assert_eq!(wrote.status.code, 0, "{}", wrote.stderr_text());
    sb.stop().await.unwrap();
    let disks = |dir: &Path| -> Vec<String> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "upper.ext4" || name.starts_with(".grow-") {
                found.push(name);
            }
        }
        found.sort();
        found
    };
    let mut at_most = Vec::new();
    for mib in [8192_u32, 16384] {
        harness
            .scope(Box::pin(async {
                SdkSandbox::get(n.as_str())
                    .await?
                    .modify()
                    .root_disk_size(mib)
                    .next_start()
                    .apply()
                    .await
            }))
            .await
            .expect("grow the stopped root");
        let grown = disks(&dir);
        let sb = rt.start(&n).await.expect("start after the grow");
        let size = sh(&sb, "df -k / | tail -1 | tr -s ' ' | cut -d' ' -f2").await;
        let seed = sh(&sb, "cat /root/seed").await;
        sb.stop().await.unwrap();
        at_most.push((
            mib,
            grown,
            disks(&dir),
            size.stdout_text().into_owned(),
            seed.stdout_text().into_owned(),
        ));
    }
    cleanup(&rt, &[&n]).await;
    for (mib, grown, kept, size, seed) in at_most {
        assert!(
            grown.len() >= 2,
            "msb left no old image after the grow to {mib} MiB: {grown:?}"
        );
        assert_eq!(kept.len(), 1, "after the start at {mib} MiB: {kept:?}");
        assert!(kept[0].starts_with(".grow-"), "{kept:?}");
        let kib: u64 = size.trim().parse().unwrap();
        assert!(
            kib > u64::from(mib) * 1024 * 9 / 10,
            "df {kib} KiB at {mib} MiB"
        );
        assert_eq!(seed, "orphan-seed\n");
    }
}
