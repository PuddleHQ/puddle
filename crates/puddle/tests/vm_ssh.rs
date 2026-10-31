// SPDX-License-Identifier: GPL-3.0-or-later
//! The SSH bar on real microVMs (tier K on Linux KVM, W on Windows WHP): the system OpenSSH
//! client runs `puddle ssh-bridge` as its `ProxyCommand`, the bridge reaches a Puddle
//! [`SshEndpoint`], and the endpoint serves each client with msb's own SSH server
//! (`Sandbox::serve_ssh`) on a sandbox that booted through the boot hook, so the gate is real.
//!
//! On Windows the client is Windows OpenSSH (`System32\OpenSSH`), never the MSYS `ssh` that Git
//! for Windows puts first on a runner's `PATH`.
//!
//! - `vm_ssh_through_the_bridge`: exit codes, scp (SFTP and legacy `-O`), an sftp batch, a local
//!   forward, and the message a client gets once the sandbox has stopped.
//! - `vm_soak_ssh_idle_15_min`: one session idle for 15 minutes still answers. Named `vm_soak_`
//!   so the per-pull-request jobs skip it (it would add 15 minutes to a partition); the nightly
//!   run and a manual dispatch of the VM workflows run it.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test; progress goes to the log"
)]

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

use puddle_boot::{BootHook, BootPlan, Gate, GatedSandbox, with_boot_mounts, write_assets};
use puddle_compute::{ExecRequest, Runtime, SandboxSpec};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_ipc::IpcRoot;
use puddle_ssh::SshEndpoint;
use puddle_types::{ImageRef, SandboxName};
use puddle_vm_tests::Settings;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::TcpStream;
use tokio::process::Command;
use tokio::time::{sleep, timeout};

/// Has `node` (the guest echo server), `scp` (legacy mode) and a login shell; runs as root.
const IMAGE: &str = "node:22-bookworm";
const CLIENT_WAIT: Duration = Duration::from_secs(120);
const IDLE: Duration = Duration::from_mins(15);

/// The OpenSSH client tools: Windows OpenSSH by full path on Windows, `PATH` elsewhere.
fn tool(name: &str) -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        PathBuf::from(root)
            .join("System32")
            .join("OpenSSH")
            .join(format!("{name}.exe"))
    } else {
        PathBuf::from(name)
    }
}

/// ssh config values want forward slashes: its parser treats a backslash as an escape.
fn slashes(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The `ProxyCommand` tokens go to the shell (`cmd.exe` on Windows) as one string: quoted only
/// when they hold a space, because `cmd /c "a" "b"` mangles two quoted tokens.
fn shell_word(text: &str) -> String {
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

fn puddle_bin() -> String {
    std::env::var("CARGO_BIN_EXE_puddle")
        .expect("CARGO_BIN_EXE_puddle: run through cargo or nextest (it sets the built binary)")
}

fn payload(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 7).to_le_bytes()[0])
        .collect()
}

/// The guest echo server a forward is checked against.
const ECHO_SERVER: &str =
    "require('net').createServer((s) => s.pipe(s)).listen(9000, '127.0.0.1');";

struct Rig {
    rt: MsbRuntime,
    name: SandboxName,
    sandbox: Arc<GatedSandbox<puddle_compute_msb::MsbSandbox>>,
    endpoint: SshEndpoint,
    /// Client files: keys, config, known hosts; also the cwd of the clients (relative paths keep
    /// `C:\...` away from scp, which would read `C` as a host).
    dir: tempfile::TempDir,
    config: PathBuf,
}

async fn keygen(path: &Path) {
    let out = Command::new(tool("ssh-keygen"))
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "puddle-vm-test",
            "-f",
        ])
        .arg(path)
        .output()
        .await
        .expect("ssh-keygen runs");
    assert!(out.status.success(), "ssh-keygen: {out:?}");
}

/// Boots `IMAGE` through the boot hook with a key authorised, serves its endpoint and writes the
/// client config (`Host box`).
async fn rig(tag: &str) -> Rig {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("id");
    keygen(&key).await;
    let public = std::fs::read_to_string(dir.path().join("id.pub")).unwrap();

    let settings = Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM test settings");
    let pair = settings.prepare().expect("msb runtime pair");
    let home = settings.home();
    let config = MsbConfig::new(&home, pair.msb, pair.libkrunfw, home.join("guest-share"))
        .with_runtime_log_level(settings.msb_log_level.clone())
        .with_keep_logs_dir(settings.kept_logs())
        .with_ssh_key(public.trim());
    let rt = MsbRuntime::open(config).await.expect("open msb");

    let image = ImageRef::new(IMAGE).unwrap();
    let image_config = rt.pull_image(&image).await.unwrap();
    let plan = BootPlan::builder(&image_config).build().unwrap();
    let assets_dir = rt.config().guest_share.join(format!("boot-{tag}"));
    let assets = write_assets(&assets_dir).unwrap();
    let agent = assets_dir.join("puddle-agent");
    std::fs::copy(
        std::env::var_os("PUDDLE_AGENT_BIN")
            .expect("PUDDLE_AGENT_BIN: the static puddle-agent (ci/build-agent.sh)"),
        &agent,
    )
    .expect("copy the agent into the guest-share root");
    let name = SandboxName::new(&format!("{}-ssh-{tag}", settings.prefix)).unwrap();
    let spec = with_boot_mounts(SandboxSpec::new(name.clone(), image), assets, Some(&agent));
    let sandbox = Arc::new(
        BootHook::new()
            .create(&rt, spec, &plan, &Gate::new())
            .await
            .unwrap(),
    );

    let root = IpcRoot::new().unwrap();
    let endpoint = SshEndpoint::start(root.listen().unwrap(), Arc::clone(&sandbox)).unwrap();
    let config_path = dir.path().join("ssh_config");
    std::fs::write(
        &config_path,
        format!(
            "Host box\n  User root\n  ProxyCommand {puddle} ssh-bridge {endpoint}\n  \
             IdentityFile {id}\n  IdentitiesOnly yes\n  IdentityAgent none\n  \
             UserKnownHostsFile {kh}\n  StrictHostKeyChecking accept-new\n  BatchMode yes\n  \
             LogLevel ERROR\n",
            puddle = shell_word(&puddle_bin()),
            endpoint = shell_word(&endpoint.endpoint().path().display().to_string()),
            id = slashes(&key),
            kh = slashes(&dir.path().join("known_hosts")),
        ),
    )
    .unwrap();
    Rig {
        rt,
        name,
        sandbox,
        endpoint,
        dir,
        config: config_path,
    }
}

impl Rig {
    fn cfg(&self) -> String {
        slashes(&self.config)
    }

    /// Runs a client tool in the client directory with stdin closed.
    async fn run(&self, program: &str, args: &[&str]) -> Output {
        timeout(
            CLIENT_WAIT,
            Command::new(tool(program))
                .current_dir(self.dir.path())
                .args(args)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap_or_else(|_| panic!("{program} {args:?} timed out"))
        .unwrap()
    }

    async fn guest(&self, script: &str) -> (i32, String) {
        let out = self
            .sandbox
            .exec(ExecRequest::sh(script).as_user("root"))
            .await
            .unwrap();
        (out.status.code, out.stdout_text().trim_end().to_owned())
    }

    async fn finish(self) {
        self.endpoint.close().await;
        let _ = self.sandbox.stop().await;
        self.rt.remove(&self.name).await.unwrap();
    }
}

fn text(out: &Output) -> String {
    format!(
        "status {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

async fn exit_codes(rig: &Rig) {
    let cfg = rig.cfg();
    let ok = rig.run("ssh", &["-F", &cfg, "box", "echo hello"]).await;
    assert_eq!(ok.status.code(), Some(0), "{}", text(&ok));
    assert_eq!(ok.stdout, b"hello\n", "{}", text(&ok));
    let seven = rig.run("ssh", &["-F", &cfg, "box", "exit 7"]).await;
    assert_eq!(seven.status.code(), Some(7), "{}", text(&seven));
    let id = rig.run("ssh", &["-F", &cfg, "box", "id -un; pwd"]).await;
    assert_eq!(id.status.code(), Some(0), "{}", text(&id));
    eprintln!(
        "exit codes: ok; guest says {:?}",
        String::from_utf8_lossy(&id.stdout)
    );
}

async fn copies(rig: &Rig) {
    let cfg = rig.cfg();
    let data = payload(8 * 1024 * 1024);
    let d = rig.dir.path();
    std::fs::write(d.join("src.bin"), &data).unwrap();
    // Relative local names, absolute guest names.
    for (flags, name) in [(vec![], "scp"), (vec!["-O"], "scp-legacy")] {
        let remote = format!("box:/tmp/{name}.bin");
        let back = format!("{name}-back.bin");
        let mut args = vec!["-F", &cfg, "-q"];
        args.extend(flags.iter().copied());
        let up = rig
            .run("scp", &[&args[..], &["src.bin", &remote]].concat())
            .await;
        assert_eq!(up.status.code(), Some(0), "{name} up: {}", text(&up));
        let down = rig
            .run("scp", &[&args[..], &[&remote, &back]].concat())
            .await;
        assert_eq!(down.status.code(), Some(0), "{name} down: {}", text(&down));
        assert!(
            std::fs::read(d.join(&back)).unwrap() == data,
            "{name} changed the file"
        );
        eprintln!("{name}: 8 MiB up and back, equal");
    }
    std::fs::write(
        d.join("batch"),
        "put src.bin /tmp/sftp.bin\nget /tmp/sftp.bin sftp-got.bin\n",
    )
    .unwrap();
    let sftp = rig
        .run("sftp", &["-F", &cfg, "-q", "-b", "batch", "box"])
        .await;
    assert_eq!(sftp.status.code(), Some(0), "sftp: {}", text(&sftp));
    assert!(
        std::fs::read(d.join("sftp-got.bin")).unwrap() == data,
        "sftp changed the file"
    );
    // The guest holds the bytes too, not only the client's copies.
    let (code, sum) = rig
        .guest("cmp /tmp/scp.bin /tmp/sftp.bin && wc -c </tmp/sftp.bin")
        .await;
    assert_eq!((code, sum.as_str()), (0, "8388608"));
    eprintln!("sftp batch: equal");
}

async fn local_forward(rig: &Rig) {
    // The echo server runs in the guest, started by exec and left running.
    let (code, out) = rig
        .guest(&format!(
            "cat >/tmp/echo.js <<'JS'\n{ECHO_SERVER}\nJS\nnohup node /tmp/echo.js >/tmp/echo.log 2>&1 &\nsleep 1; echo started"
        ))
        .await;
    assert_eq!((code, out.as_str()), (0, "started"));
    let local_port = {
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let forward = format!("{local_port}:127.0.0.1:9000");
    let ssh = Command::new(tool("ssh"))
        .current_dir(rig.dir.path())
        .args([
            "-F",
            &rig.cfg(),
            "-N",
            "-o",
            "ExitOnForwardFailure=yes",
            "-L",
        ])
        .arg(&forward)
        .arg("box")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let ssh = std::cell::RefCell::new(ssh);
    let mut conn = puddle_test_kit::eventually_async(
        "the forward's local port opens",
        Duration::from_secs(100),
        || async {
            let connected = TcpStream::connect(("127.0.0.1", local_port)).await;
            if connected.is_err() {
                assert!(
                    ssh.borrow_mut().try_wait().unwrap().is_none(),
                    "ssh -L exited early"
                );
            }
            connected
        },
    )
    .await;
    let mut ssh = ssh.into_inner();
    let data = payload(1024 * 1024);
    let (mut r, mut w) = conn.split();
    let mut back = vec![0_u8; data.len()];
    let ((), ()) = timeout(CLIENT_WAIT, async {
        tokio::join!(async { w.write_all(&data).await.unwrap() }, async {
            r.read_exact(&mut back).await.unwrap();
        })
    })
    .await
    .expect("1 MiB echoes through the forward");
    assert!(back == data, "the forward changed the data");
    ssh.kill().await.unwrap();
    eprintln!("local forward: 1 MiB echoed intact");
}

/// `puddle ssh-bridge` has no more to relay once the sandbox is stopped: the client reads the
/// reason the endpoint gives.
async fn refused_after_stop(rig: &Rig) {
    rig.sandbox.stop().await.unwrap();
    let out = rig.run("ssh", &["-F", &rig.cfg(), "box", "true"]).await;
    let all = text(&out);
    assert_ne!(out.status.code(), Some(0), "{all}");
    assert!(
        all.contains("puddle:") && all.contains("not running"),
        "{all}"
    );
    eprintln!(
        "after stop: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_ssh_through_the_bridge() {
    let rig = rig("bridge").await;
    exit_codes(&rig).await;
    copies(&rig).await;
    local_forward(&rig).await;
    refused_after_stop(&rig).await;
    rig.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_ssh_idle_15_min() {
    let rig = rig("idle").await;
    let mut ssh = Command::new(tool("ssh"))
        .current_dir(rig.dir.path())
        .args(["-F", &rig.cfg(), "box", "read l; echo got:$l"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = ssh.stdin.take().unwrap();
    let mut stdout = BufReader::new(ssh.stdout.take().unwrap());
    let started = std::time::Instant::now();
    // Prove the session is up before idling: a second client works while the first waits.
    let probe = rig
        .run("ssh", &["-F", &rig.cfg(), "box", "echo second"])
        .await;
    assert_eq!(probe.stdout, b"second\n", "{}", text(&probe));
    // flake-ok: an idle window checked once a minute that the session stays up, not a wait for a condition
    while started.elapsed() < IDLE {
        sleep(Duration::from_secs(60)).await;
        assert!(
            ssh.try_wait().unwrap().is_none(),
            "the idle session ended after {:?}",
            started.elapsed()
        );
        eprintln!("idle {} min", started.elapsed().as_secs() / 60);
    }
    stdin.write_all(b"after-idle\n").await.unwrap();
    stdin.flush().await.unwrap();
    let mut line = String::new();
    timeout(CLIENT_WAIT, stdout.read_line(&mut line))
        .await
        .expect("the idle session answers")
        .unwrap();
    assert_eq!(line, "got:after-idle\n");
    drop(stdin);
    let status = timeout(CLIENT_WAIT, ssh.wait()).await.unwrap().unwrap();
    assert_eq!(status.code(), Some(0));
    // A new client after the idle works as well.
    let again = rig
        .run("ssh", &["-F", &rig.cfg(), "box", "echo third"])
        .await;
    assert_eq!(again.stdout, b"third\n", "{}", text(&again));
    eprintln!("idle 15 min: answered after {:?}", started.elapsed());
    rig.finish().await;
}
