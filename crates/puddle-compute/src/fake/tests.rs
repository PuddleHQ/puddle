// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake's own controls and the built-ins; the shared behaviour is in the contract suite.

use std::time::Duration;

use puddle_types::{GuestEnv, GuestPath, ImageRef, SandboxName, VolumeName, WorkspaceStatus};

use super::*;
use crate::{FileMount, OwnedDisk, VolumeMount};

fn name(s: &str) -> SandboxName {
    SandboxName::new(s).unwrap()
}

fn path(s: &str) -> GuestPath {
    GuestPath::new(s).unwrap()
}

fn vol(s: &str) -> VolumeName {
    VolumeName::new(s).unwrap()
}

fn spec(n: &str) -> SandboxSpec {
    SandboxSpec::new(name(n), ImageRef::new(FakeRuntime::DEBIAN).unwrap())
}

fn boom(op: &'static str) -> ComputeError {
    ComputeError::Runtime {
        op,
        message: "injected".into(),
    }
}

async fn run(sb: &FakeSandbox, program: &str, args: &[&str]) -> ExecOutput {
    sb.exec(ExecRequest::new(program, args.iter().copied()))
        .await
        .unwrap()
}

#[tokio::test]
async fn fault_once_fails_one_call_then_clears() {
    let rt = FakeRuntime::new();
    rt.inject(Op::Probe, Fault::once(boom("probe")));
    assert_eq!(rt.probe().await.unwrap_err(), boom("probe"));
    assert!(rt.probe().await.is_ok());
}

#[tokio::test]
async fn fault_after_times_and_always() {
    let rt = FakeRuntime::new();
    rt.inject(Op::List, Fault::once(boom("list")).after(1).times(2));
    assert!(rt.list().await.is_ok());
    assert!(rt.list().await.is_err());
    assert!(rt.list().await.is_err());
    assert!(rt.list().await.is_ok());
    rt.inject(Op::List, Fault::always(boom("list")));
    for _ in 0..5 {
        assert!(rt.list().await.is_err());
    }
    rt.clear_faults();
    assert!(rt.list().await.is_ok());
}

#[tokio::test]
async fn fault_only_for_matches_its_target() {
    let rt = FakeRuntime::new();
    rt.inject(Op::Create, Fault::once(boom("create")).only_for("b"));
    let _a = rt.create(spec("a")).await.unwrap();
    assert_eq!(rt.create(spec("b")).await.unwrap_err(), boom("create"));
    let _b = rt.create(spec("b")).await.unwrap();
}

#[tokio::test]
async fn every_operation_can_fail() {
    let rt = FakeRuntime::new();
    let sb = rt.create(spec("a")).await.unwrap();
    let n = name("a");
    let v = vol("v");
    let ops = [
        Op::Probe,
        Op::PullImage,
        Op::Create,
        Op::Start,
        Op::Get,
        Op::SetMemory,
        Op::SetCpus,
        Op::SetEnv,
        Op::GrowRootDisk,
        Op::List,
        Op::Remove,
        Op::StaleDirs,
        Op::RemoveStaleDir,
        Op::CreateVolume,
        Op::Volume,
        Op::ListVolumes,
        Op::RemoveVolume,
        Op::Status,
        Op::Stop,
        Op::Exec,
        Op::ServeSsh,
    ];
    for op in ops {
        rt.inject(op, Fault::once(boom("x")));
    }
    let image = ImageRef::new(FakeRuntime::DEBIAN).unwrap();
    let vspec = VolumeSpec {
        name: v.clone(),
        size: DiskSize::mib(1),
    };
    let (_client, server) = tokio::io::duplex(64);
    let results = [
        rt.probe().await.err(),
        rt.pull_image(&image).await.err(),
        rt.create(spec("b")).await.err(),
        rt.start(&n).await.err(),
        rt.get(&n).await.err(),
        rt.set_memory(&n, MemoryMib::MIN).await.err(),
        rt.set_cpus(&n, CpuCount::MIN).await.err(),
        rt.set_env(&n, &GuestEnv::new()).await.err(),
        rt.grow_root_disk(&n, DiskSize::mib(99_999)).await.err(),
        rt.list().await.err(),
        rt.remove(&n).await.err(),
        rt.stale_dirs().await.err(),
        rt.remove_stale_dir(&n).await.err(),
        rt.create_volume(vspec).await.err(),
        rt.volume(&v).await.err(),
        rt.list_volumes().await.err(),
        rt.remove_volume(&v).await.err(),
        sb.status().await.err(),
        sb.stop().await.err(),
        sb.exec(ExecRequest::sh("exit 0")).await.err(),
        sb.serve_ssh(server).await.err(),
    ];
    for (op, result) in ops.iter().zip(results) {
        assert_eq!(result, Some(boom("x")), "{op:?}");
    }
    assert_eq!(sb.status().await.unwrap(), WorkspaceStatus::Running);
}

#[tokio::test]
async fn calls_are_logged_in_order_with_targets() {
    let rt = FakeRuntime::new();
    let sb = rt.create(spec("a")).await.unwrap();
    sb.exec(ExecRequest::new("fstrim", ["/workspaces"]))
        .await
        .unwrap();
    sb.stop().await.unwrap();
    let calls = rt.calls();
    let ops: Vec<Op> = calls.iter().map(|c| c.op).collect();
    assert_eq!(ops, [Op::Create, Op::Exec, Op::Stop]);
    assert!(calls.iter().all(|c| c.target.as_deref() == Some("a")));
    assert_eq!(calls[1].detail.as_deref(), Some("fstrim /workspaces"));
}

#[tokio::test]
async fn handlers_run_before_builtins_newest_first() {
    let rt = FakeRuntime::new();
    rt.on_exec(|_: &mut ExecContext<'_>, r: &ExecRequest| {
        (r.program == "fstrim").then(|| ExecOutput::new(0, "old", ""))
    });
    rt.on_exec(|ctx: &mut ExecContext<'_>, r: &ExecRequest| {
        (r.program == "fstrim").then(|| ExecOutput::new(0, format!("new {}", ctx.sandbox()), ""))
    });
    rt.on_exec(|ctx: &mut ExecContext<'_>, r: &ExecRequest| {
        (r.program == "true").then(|| {
            ctx.write(&GuestPath::new("/hook-ran").unwrap(), b"y".to_vec())
                .unwrap();
            ExecOutput::new(5, "", "")
        })
    });
    let sb = rt.create(spec("a")).await.unwrap();
    assert_eq!(run(&sb, "fstrim", &[]).await.stdout_text(), "new a");
    assert_eq!(run(&sb, "true", &[]).await.status.code, 5);
    assert_eq!(run(&sb, "cat", &["/hook-ran"]).await.stdout_text(), "y");
}

#[tokio::test]
async fn handlers_see_the_merged_env() {
    let rt = FakeRuntime::new();
    rt.on_exec(|ctx: &mut ExecContext<'_>, r: &ExecRequest| {
        (r.program == "env-count").then(|| ExecOutput::new(0, ctx.env().len().to_string(), ""))
    });
    let mut env = puddle_types::GuestEnv::new();
    env.set("A", "1").unwrap();
    let sb = rt.create(spec("a").with_env(&env)).await.unwrap();
    let mut more = puddle_types::GuestEnv::new();
    more.set("B", "2").unwrap();
    let out = sb
        .exec(ExecRequest::new("env-count", Vec::<String>::new()).with_env(&more))
        .await
        .unwrap();
    assert_eq!(out.stdout_text(), "2");
}

#[tokio::test]
async fn builtins() {
    let rt = FakeRuntime::new();
    let mut env = puddle_types::GuestEnv::new();
    env.set("A", "1").unwrap();
    let sb = rt.create(spec("a").with_env(&env)).await.unwrap();
    assert_eq!(run(&sb, "/bin/true", &[]).await.status.code, 0);
    assert_eq!(run(&sb, "false", &[]).await.status.code, 1);
    assert_eq!(run(&sb, "echo", &["a", "b"]).await.stdout_text(), "a b\n");
    assert_eq!(run(&sb, "printenv", &[]).await.stdout_text(), "A=1\n");
    assert_eq!(run(&sb, "printenv", &["NOPE"]).await.status.code, 1);
    assert_eq!(run(&sb, "sh", &["-c", ""]).await.status.code, 0);
    assert_eq!(run(&sb, "sh", &["-c", "exit"]).await.status.code, 0);
    assert_eq!(run(&sb, "sh", &["-c", "exit x"]).await.status.code, 2);
    assert_eq!(
        run(&sb, "bash", &["-c", "kill -KILL $$"]).await.status.code,
        -1
    );
    assert_eq!(
        run(&sb, "sh", &["-c", "echo hi"]).await.stdout_text(),
        "hi\n"
    );
    let unknown = run(&sb, "frobnicate", &[]).await;
    assert_eq!(unknown.status.code, 127);
    assert!(unknown.stderr_text().contains("frobnicate: not found"));
    assert_eq!(run(&sb, "test", &["-e", "/"]).await.status.code, 0);
    assert_eq!(run(&sb, "test", &["-f", "/nope"]).await.status.code, 1);
    assert_eq!(run(&sb, "test", &["-e", "relative"]).await.status.code, 1);
    assert_eq!(run(&sb, "sleep", &["1"]).await.status.code, 0);
    assert_eq!(run(&sb, "sleep", &["soon"]).await.status.code, 1);
    let missing = run(&sb, "cat", &["/a", "rel"]).await;
    assert_eq!(missing.status.code, 1);
    assert!(missing.stderr_text().contains("/a: No such file"));
    assert!(missing.stderr_text().contains("rel: No such file"));
    let tee = sb
        .exec(ExecRequest::new("tee", ["rel"]).with_stdin("x"))
        .await
        .unwrap();
    assert_eq!(tee.status.code, 1);
}

#[tokio::test]
async fn sleep_longer_than_the_timeout_times_out() {
    let rt = FakeRuntime::new();
    let sb = rt.create(spec("a")).await.unwrap();
    let err = sb
        .exec(ExecRequest::new("sleep", ["0.5"]).with_timeout(Duration::from_millis(100)))
        .await
        .unwrap_err();
    assert!(matches!(err, ComputeError::ExecTimeout { ref program, .. } if program == "sleep"));
}

#[tokio::test]
async fn filesystem_layout() {
    let rt = FakeRuntime::new();
    let tmp = tempfile::tempdir().unwrap();
    let host_dir = tmp.path().to_path_buf();
    let present = host_dir.join("present");
    std::fs::write(&present, b"p").unwrap();
    let s = spec("a")
        .with_file_mount(FileMount::read_only(&present, path("/m/present")))
        .with_file_mount(FileMount::read_only(
            host_dir.join("absent"),
            path("/m/absent"),
        ))
        .with_file_mount(FileMount::read_only(&host_dir, path("/m/dir")))
        .with_volume(VolumeMount::named(vol("v"), path("/w")).ensure_size(DiskSize::mib(1)))
        .with_owned_disk(OwnedDisk {
            guest: path("/var/lib/d"),
            size: DiskSize::mib(1),
        });
    let sb = rt.create(s).await.unwrap();
    let exists = |p: &'static str| {
        let sb = &sb;
        async move { run(sb, "test", &["-e", p]).await.status.success() }
    };
    assert!(exists("/m/present").await);
    assert!(!exists("/m/absent").await);
    assert!(!exists("/m/present/below").await);
    assert!(exists("/m").await, "parent of a mount");
    assert!(exists("/var/lib").await, "parent of an owned disk");
    assert!(exists("/w").await, "volume mount point");
    assert!(!exists("/w/x").await);
    assert_eq!(run(&sb, "cat", &["/m/absent"]).await.status.code, 1);
    let dir_read = run(&sb, "cat", &["/m/dir"]).await;
    assert_eq!(dir_read.status.code, 1, "reading a directory fails");
    assert!(!dir_read.stderr_text().contains("No such file"));
    let write = |p: &'static str| {
        let sb = &sb;
        async move {
            sb.exec(ExecRequest::new("tee", [p]).with_stdin("d"))
                .await
                .unwrap()
        }
    };
    assert!(
        write("/m/present/below")
            .await
            .stderr_text()
            .contains("No such file")
    );
    assert!(write("/w").await.stderr_text().contains("No such file"));
    assert!(write("/w/sub/f").await.status.success());
    assert!(exists("/w/sub").await, "directory implied by a file");
    assert!(write("/var/lib/d/f").await.status.success());
    assert!(exists("/var/lib/d/f").await);
}

#[tokio::test]
async fn a_directory_mount_shows_its_tree_read_only() {
    let rt = FakeRuntime::new();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("top"), b"t").unwrap();
    std::fs::write(root.join("sub/deep"), b"d").unwrap();
    let s = spec("a")
        .with_file_mount(FileMount::read_only_dir(&root, path("/cache/tree")))
        .with_file_mount(FileMount::read_only(root.join("top"), path("/cache/file")));
    let sb = rt.create(s).await.unwrap();
    assert_eq!(run(&sb, "cat", &["/cache/tree/top"]).await.stdout, b"t");
    assert_eq!(
        run(&sb, "cat", &["/cache/tree/sub/deep"]).await.stdout,
        b"d"
    );
    assert_eq!(run(&sb, "cat", &["/cache/tree/nope"]).await.status.code, 1);
    // A path beside the directory mount is not part of it.
    assert_eq!(run(&sb, "cat", &["/cache/file"]).await.stdout, b"t");
    for p in ["/cache/tree", "/cache/tree/sub", "/cache/tree/top"] {
        assert!(run(&sb, "test", &["-e", p]).await.status.success(), "{p}");
    }
    assert!(
        !run(&sb, "test", &["-e", "/cache/tree/nope"])
            .await
            .status
            .success()
    );
    for target in ["/cache/tree/top", "/cache/tree/new", "/cache/tree/sub/new"] {
        let out = sb
            .exec(ExecRequest::new("tee", [target]).with_stdin("x"))
            .await
            .unwrap();
        assert_eq!(out.status.code, 1, "{target}");
        assert!(
            out.stderr_text().contains("Read-only file system"),
            "{target}"
        );
    }
    assert_eq!(std::fs::read(root.join("top")).unwrap(), b"t");
    assert!(!root.join("new").exists());
}

#[tokio::test]
async fn a_checkout_must_look_like_a_git_directory_and_its_links_lead_nowhere() {
    let rt = FakeRuntime::new();
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("api");
    let git = work.join(".git");
    std::fs::create_dir_all(git.join("objects")).unwrap();
    std::fs::create_dir_all(git.join("refs")).unwrap();
    std::fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
    std::fs::write(work.join(".env"), b"SECRET=1\n").unwrap();
    let mount = |host: &std::path::Path| {
        spec("import").with_file_mount(FileMount::checkout(host, path("/puddle/import")))
    };
    // The working tree is no checkout mount, whatever the caller meant.
    let err = rt.create(mount(&work)).await.unwrap_err().to_string();
    assert!(err.contains("is not a `.git` directory"), "{err}");
    std::fs::write(git.join("commondir"), b"../..\n").unwrap();
    let err = rt.create(mount(&git)).await.unwrap_err().to_string();
    assert!(err.contains("linked worktree"), "{err}");
    std::fs::remove_file(git.join("commondir")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(work.join(".env"), git.join("escape")).unwrap();
    let sb = rt.create(mount(&git)).await.unwrap();
    assert_eq!(
        run(&sb, "cat", &["/puddle/import/HEAD"]).await.stdout,
        b"ref: refs/heads/main\n"
    );
    #[cfg(unix)]
    assert_eq!(
        run(&sb, "cat", &["/puddle/import/escape"])
            .await
            .status
            .code,
        1
    );
    // A path beside the checkout is not part of it.
    assert_eq!(run(&sb, "cat", &["/puddle/other"]).await.status.code, 1);
    let out = sb
        .exec(ExecRequest::new("tee", ["/puddle/import/HEAD"]).with_stdin("x"))
        .await
        .unwrap();
    assert!(out.stderr_text().contains("Read-only file system"));
}

#[tokio::test]
async fn writes_into_a_removed_volume_fail() {
    let rt = FakeRuntime::new();
    let s = spec("a")
        .with_volume(VolumeMount::named(vol("v"), path("/w")).ensure_size(DiskSize::mib(1)));
    let sb = rt.create(s).await.unwrap();
    rt.inner.state.lock().unwrap().volumes.remove("v");
    let out = sb
        .exec(ExecRequest::new("tee", ["/w/f"]).with_stdin("d"))
        .await
        .unwrap();
    assert_eq!(out.status.code, 1);
    assert_eq!(run(&sb, "cat", &["/w/f"]).await.status.code, 1);
    assert!(!run(&sb, "test", &["-e", "/w/f"]).await.status.success());
}

#[tokio::test]
async fn crash_and_drop_semantics() {
    let rt = FakeRuntime::new();
    let n = name("a");
    let sb = rt.create(spec("a")).await.unwrap();
    assert!(rt.crash(&n));
    assert!(!rt.crash(&n), "already down");
    assert!(!rt.crash(&name("nope")));
    assert_eq!(sb.status().await.unwrap(), WorkspaceStatus::Crashed);
    let restarted = rt.start(&n).await.unwrap();
    drop(sb);
    assert_eq!(
        restarted.status().await.unwrap(),
        WorkspaceStatus::Running,
        "dropping the earlier boot's handle must not touch the new boot"
    );
    let err = rt.start(&n).await.unwrap_err();
    assert!(matches!(
        err,
        ComputeError::InvalidState { op: "start", .. }
    ));
}

#[tokio::test]
async fn stale_handles_cannot_stop_a_newer_boot() {
    let rt = FakeRuntime::new();
    let n = name("a");
    let first = rt.create(spec("a")).await.unwrap();
    assert!(rt.crash(&n));
    let _second = rt.start(&n).await.unwrap();
    assert!(matches!(
        first.stop().await,
        Err(ComputeError::StaleHandle { .. })
    ));
    let (_c, server) = tokio::io::duplex(64);
    assert!(matches!(
        first.serve_ssh(server).await,
        Err(ComputeError::StaleHandle { .. })
    ));
}

#[tokio::test]
async fn handles_of_removed_sandboxes_report_not_found() {
    let rt = FakeRuntime::new();
    let sb = rt.create(spec("a")).await.unwrap();
    sb.stop().await.unwrap();
    rt.remove(&name("a")).await.unwrap();
    assert!(matches!(
        sb.status().await,
        Err(ComputeError::NotFound { .. })
    ));
    assert!(matches!(
        sb.stop().await,
        Err(ComputeError::NotFound { .. })
    ));
    assert!(matches!(
        sb.exec(ExecRequest::sh("exit 0")).await,
        Err(ComputeError::NotFound { .. })
    ));
}

#[tokio::test]
async fn start_checks_volumes_again() {
    let rt = FakeRuntime::new();
    let mount = |n: &str| {
        spec(n).with_volume(VolumeMount::named(vol("v"), path("/w")).ensure_size(DiskSize::mib(1)))
    };
    let a = rt.create(mount("a")).await.unwrap();
    a.stop().await.unwrap();
    let b = rt.create(mount("b")).await.unwrap();
    let err = rt.start(&name("a")).await.unwrap_err();
    assert_eq!(
        err,
        ComputeError::VolumeInUse {
            volume: "v".into(),
            holder: "b".into()
        }
    );
    drop(b);
    rt.remove(&name("b")).await.unwrap();
    rt.remove_volume(&vol("v")).await.unwrap();
    let err = rt.start(&name("a")).await.unwrap_err();
    assert!(matches!(err, ComputeError::VolumeNotFound { .. }));
}

#[tokio::test]
async fn images_and_probe() {
    let rt = FakeRuntime::with_config(FakeConfig {
        runtime_version: "0.7.7-puddle.1".into(),
    });
    let caps = rt.probe().await.unwrap();
    assert_eq!(caps.runtime_version, "0.7.7-puddle.1");
    assert!(caps.stale_dir_fixed && caps.ssh_reports_signal_exit);
    let dind = rt
        .pull_image(&ImageRef::new(FakeRuntime::DIND).unwrap())
        .await
        .unwrap();
    assert_eq!(dind.entrypoint, ["dockerd-entrypoint.sh"]);
    let custom = ImageRef::new("example.invalid/custom:1").unwrap();
    assert!(rt.pull_image(&custom).await.is_err());
    rt.add_image(
        &custom,
        ImageConfig {
            cmd: vec!["x".into()],
            ..ImageConfig::default()
        },
    );
    assert_eq!(rt.pull_image(&custom).await.unwrap().cmd, ["x"]);
    assert!(format!("{rt:?}").contains("0.7.7-puddle.1"));
    assert!(format!("{:?}", FakeRuntime::default()).contains("fake"));
}

#[tokio::test]
async fn volume_size_zero_is_refused() {
    let rt = FakeRuntime::new();
    let err = rt
        .create_volume(VolumeSpec {
            name: vol("v"),
            size: DiskSize::mib(0),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, ComputeError::InvalidSpec { .. }));
}

/// A stream whose every read and write fails.
struct Broken;
impl tokio::io::AsyncRead for Broken {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("read broke")))
    }
}
impl tokio::io::AsyncWrite for Broken {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::other("write broke")))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn ssh_reports_io_errors() {
    let rt = FakeRuntime::new();
    let sb = rt.create(spec("a")).await.unwrap();
    let err = sb.serve_ssh(Broken).await.unwrap_err();
    assert!(err.to_string().contains("write broke"), "{err}");
    // The terminal stand-in reports a connection that breaks the same way.
    rt.serve_terminals();
    let err = sb.serve_ssh(Broken).await.unwrap_err();
    assert!(
        matches!(
            err,
            ComputeError::Runtime {
                op: "serve ssh",
                ..
            }
        ),
        "{err}"
    );
}

struct AnyHost;

impl russh::client::Handler for AnyHost {
    type Error = russh::Error;

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the trait's method is async; the answer is known"
    )]
    async fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

#[tokio::test]
async fn the_terminal_stand_in_runs_an_unknown_line_and_ends_cleanly_when_the_client_leaves_a_flood()
 {
    let (near, far) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(pty::serve(far));
    let mut client = russh::client::connect_stream(Arc::default(), near, AnyHost)
        .await
        .unwrap();
    let key = russh::keys::PrivateKey::from(
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[1; 32]),
    );
    let auth = client
        .authenticate_publickey(
            "agent",
            russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None),
        )
        .await
        .unwrap();
    assert!(auth.success());
    let mut channel = client.channel_open_session().await.unwrap();
    channel
        .request_pty(false, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    channel.request_shell(false).await.unwrap();
    channel.data(&b"nothing-known\rflood\r"[..]).await.unwrap();
    let mut seen = String::new();
    while !seen.contains("yyyy") {
        match channel.wait().await {
            Some(russh::ChannelMsg::Data { data }) => {
                seen.push_str(&String::from_utf8_lossy(&data));
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(seen.contains("$ "), "{seen}");
    client
        .disconnect(russh::Disconnect::ByApplication, "bye", "en")
        .await
        .unwrap();
    drop(channel);
    // However the connection ends under a flood, the server task ends too.
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .ok();
}

#[tokio::test]
async fn foreign_sandboxes_dirs_and_volumes_are_listed_but_not_owned() {
    let rt = FakeRuntime::new();
    let _ours = rt.create(spec("ours")).await.unwrap();
    rt.add_foreign_sandbox("Other_Tool", WorkspaceStatus::Running);
    rt.add_foreign_sandbox("valid-but-foreign", WorkspaceStatus::Stopped);
    let listed = rt.list().await.unwrap();
    let owned: Vec<(&str, bool)> = listed
        .iter()
        .map(|i| (i.name.as_str(), i.puddle_owned))
        .collect();
    assert_eq!(
        owned,
        [
            ("Other_Tool", false),
            ("ours", true),
            ("valid-but-foreign", false)
        ]
    );
    // Only listed: other operations don't see a foreign sandbox.
    assert!(matches!(
        rt.get(&name("valid-but-foreign")).await,
        Err(ComputeError::NotFound { .. })
    ));

    rt.add_stale_dir("Odd_Dir");
    rt.add_stale_dir("left");
    assert_eq!(rt.stale_dirs().await.unwrap(), ["Odd_Dir", "left"]);
    rt.remove_stale_dir(&name("left")).await.unwrap();
    assert_eq!(rt.stale_dirs().await.unwrap(), ["Odd_Dir"]);

    rt.add_foreign_volume("Data_Disk", DiskSize::mib(8));
    let vols = rt.list_volumes().await.unwrap();
    assert_eq!(vols.len(), 1);
    assert_eq!(vols[0].name, "Data_Disk");
    assert_eq!(vols[0].size, DiskSize::mib(8));
    assert!(vols[0].volume_name().is_none());
    assert!(rt.volume(&vol("data-disk")).await.unwrap().is_none());
}

#[tokio::test]
async fn the_specs_root_disk_size_can_be_read_back() {
    let rt = FakeRuntime::new();
    let named = spec("a").with_root_disk(DiskSize::gib(16));
    let plain = spec("b");
    let (a, b) = (named.name.clone(), plain.name.clone());
    assert_eq!(rt.root_disk_size(&a), None, "no such sandbox yet");
    rt.create(named).await.unwrap();
    rt.create(plain).await.unwrap();
    assert_eq!(rt.root_disk_size(&a), Some(DiskSize::mib(16384)));
    assert_eq!(
        rt.root_disk_size(&b),
        None,
        "a spec with none leaves the default"
    );
}

#[tokio::test]
async fn the_specs_owned_disks_and_volume_mounts_can_be_read_back() {
    let rt = FakeRuntime::new();
    let s = spec("a")
        .with_volume(VolumeMount::named(vol("v"), path("/w")).ensure_size(DiskSize::mib(1)))
        .with_owned_disk(OwnedDisk {
            guest: path("/var/lib/d"),
            size: DiskSize::mib(2),
        });
    let name = s.name.clone();
    assert_eq!(rt.owned_disks(&name).len(), 0);
    assert_eq!(rt.volume_mounts(&name).len(), 0);
    rt.create(s).await.unwrap();
    let disks = rt.owned_disks(&name);
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0].guest, path("/var/lib/d"));
    assert_eq!(disks[0].size, DiskSize::mib(2));
    let volumes = rt.volume_mounts(&name);
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].guest, path("/w"));
}

#[tokio::test]
async fn a_running_root_disk_grows_live_and_a_stopped_one_at_the_next_start() {
    let rt = FakeRuntime::new();
    let n = name("a");
    let sb = rt
        .create(spec("a").with_root_disk(DiskSize::mib(1_000)))
        .await
        .unwrap();
    rt.set_root_disk_use(&n, DiskSize::mib(1_000), DiskSize::mib(10));
    let live = rt.grow_root_disk(&n, DiskSize::mib(2_000)).await.unwrap();
    assert_eq!(live, RootDiskGrowth::Live);
    assert_eq!(rt.root_disk_size(&n), Some(DiskSize::mib(2_000)));
    let listed = |rt: &FakeRuntime| {
        let rt = rt.clone();
        async move { rt.list().await.unwrap()[0].root_disk.unwrap() }
    };
    assert_eq!(listed(&rt).await.size, DiskSize::mib(2_000));
    assert_eq!(listed(&rt).await.used, DiskSize::mib(10));
    sb.stop().await.unwrap();
    let off = rt.grow_root_disk(&n, DiskSize::mib(3_000)).await.unwrap();
    assert_eq!(off, RootDiskGrowth::NextStart);
    assert_eq!(listed(&rt).await.size, DiskSize::mib(3_000));
}

#[tokio::test]
async fn a_runtime_that_cannot_grow_live_waits_for_the_boot() {
    let rt = FakeRuntime::new();
    rt.set_live_root_grow(false);
    let n = name("a");
    let sb = rt
        .create(spec("a").with_root_disk(DiskSize::mib(1_000)))
        .await
        .unwrap();
    rt.set_root_disk_use(&n, DiskSize::mib(1_000), DiskSize::mib(10));
    let got = rt.grow_root_disk(&n, DiskSize::mib(2_000)).await.unwrap();
    assert_eq!(got, RootDiskGrowth::NextStart);
    assert_eq!(
        rt.list().await.unwrap()[0].root_disk.unwrap().size,
        DiskSize::mib(1_000)
    );
    sb.stop().await.unwrap();
    rt.start(&n).await.unwrap();
    assert_eq!(
        rt.list().await.unwrap()[0].root_disk.unwrap().size,
        DiskSize::mib(2_000)
    );
}

#[tokio::test]
async fn a_root_disk_never_shrinks_and_an_unknown_sandbox_is_not_found() {
    let rt = FakeRuntime::new();
    let n = name("a");
    rt.create(spec("a").with_root_disk(DiskSize::mib(1_000)))
        .await
        .unwrap();
    for size in [500, 1_000] {
        let err = rt
            .grow_root_disk(&n, DiskSize::mib(size))
            .await
            .unwrap_err();
        assert!(matches!(err, ComputeError::InvalidSpec { .. }), "{err:?}");
    }
    let err = rt
        .grow_root_disk(&name("nope"), DiskSize::mib(5_000))
        .await
        .unwrap_err();
    assert!(matches!(err, ComputeError::NotFound { .. }), "{err:?}");
}

async fn sweep(sb: &FakeSandbox, cid: &str, ranges: &[&str]) -> ExecOutput {
    let mut args = vec!["-e", crate::probes::VSOCK_SWEEP, "hello", cid];
    args.extend(ranges);
    run(sb, "perl", &args).await
}

#[tokio::test]
async fn the_fake_guest_sweeps_ports_and_ranges_of_the_host_only() {
    use crate::VsockRoute;
    use crate::probes::VSOCK_SWEEP;
    let rt = FakeRuntime::new();
    let sb = rt
        .create(
            spec("a")
                .with_route(VsockRoute::new(5000, "/nonexistent/a"))
                .with_route(VsockRoute::new(5001, "/nonexistent/b")),
        )
        .await
        .unwrap();
    let out = sweep(&sb, "2", &["4990-5000", "5001", "9-1"]).await;
    assert_eq!(out.stdout_text(), "accepted 5000\naccepted 5001\n");
    assert_eq!(sweep(&sb, "2", &["1-4999"]).await.stdout_text(), "");
    assert_eq!(sweep(&sb, "3", &["1-65535"]).await.stdout_text(), "");
    assert_eq!(sweep(&sb, "2", &["x"]).await.stdout_text(), "");
    let bad_cid = sweep(&sb, "host", &["5000"]).await;
    assert_eq!(bad_cid.status.code, 2);
    let no_args = run(&sb, "perl", &["-e", VSOCK_SWEEP]).await;
    assert_eq!(no_args.status.code, 2);
}
