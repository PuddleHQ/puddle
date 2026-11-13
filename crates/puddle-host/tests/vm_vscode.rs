// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop VS Code end to end on a real microVM (Windows WHP): the pinned desktop VS Code opens a
//! workspace over Remote-SSH, through the ssh config entry the host wrote, the bridge and the
//! runtime's SSH server, and a driver clicks and types in the real window. It stands for the VS Code
//! half of the hands-on checks; nobody touches the window and no account is signed in.
//!
//! - the workspace's checkouts show as one root each in the explorer (multi-root file);
//! - the guest's git extension asks the window for a GitHub token and the window answers with a canary
//!   token a test extension hands out (it replaces the `github` provider), so the token reaches the
//!   guest as documented while no real credential is in play;
//! - Developer: Reload Window with a terminal running replaces the guest's server and ends the terminal
//!   (`remote.SSH.useLocalServer` off, the default);
//! - code the guest put in its editor server opens a local terminal on this computer and runs a command;
//! - with no `github` provider on this side the guest gets no token, and that code still runs.
//!
//! This binary is its own `ProxyCommand`, as `vm_desktop_attach` is, and runs on Windows only: Linux
//! has no desktop VS Code in the VM tier. It is not in any partition (`ci/vm-filter.sh`); one job of
//! `vm-windows.yml` runs it on the nightly and on a dispatch.
#![expect(
    clippy::print_stdout,
    reason = "a plain test program for a VM: a failed step fails the run; nothing else prints"
)]

#[cfg(windows)]
mod support;
#[cfg(windows)]
mod vm_support;

use std::process::ExitCode;

type Case = (&'static str, fn());

#[cfg(windows)]
const CASES: &[Case] = &[(
    "vm_vscode_attach_shows_the_checkouts_and_the_guest_reaches_this_computer_as_documented",
    story::attach_story,
)];

#[cfg(not(windows))]
const CASES: &[Case] = &[];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if args.first().map(String::as_str) == Some("ssh-bridge") {
        return vm_support::ssh_bridge(args.get(1).map(String::as_str));
    }
    if args.iter().any(|a| a == "--list") {
        if !args.iter().any(|a| a == "--ignored") {
            for (name, _) in CASES {
                println!("{name}: test");
            }
        }
        return ExitCode::SUCCESS;
    }
    let wanted: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    for (name, body) in CASES {
        if wanted.is_empty() || wanted.iter().any(|w| name.contains(w.as_str())) {
            body();
            println!("test {name} ... ok");
        }
    }
    ExitCode::SUCCESS
}

#[cfg(windows)]
#[expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "a plain test program for a VM: a failed step fails the run"
)]
mod story {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use puddle_compute_msb::MsbRuntime;
    use puddle_host::{Host, HostOptions, VsCodeLauncher};
    use puddle_test_kit::eventually_async;
    use puddle_types::WorkspaceId;
    use puddle_vm_tests::{
        BROWSER_MODULES_VAR, Node, VsCodeLaunch, VsCodeRelease, VsCodeWindow, local_terminal_probe,
    };
    use serde_json::json;

    use crate::support::{Api, Events, ended, named_id_options};
    use crate::vm_support::{VmHost, guest, start_vm_host_config};

    /// Not a credential: the token the test extension answers with, so a `gho_` in a guest is this
    /// and nothing else.
    const CANARY: &str = "gho_PuddleCanaryNotARealToken0000000000";

    /// How long VS Code gets to attach, and to attach again after a reload: the first start downloads
    /// nothing in the guest (the host uploads its server) but unpacks and starts it.
    const UP: Duration = Duration::from_secs(240);

    /// How long a guest-side effect gets to show (the most `eventually` allows).
    const SHOWS: Duration = Duration::from_secs(100);

    const PORT: u16 = 9230;

    /// The Machine settings of the guest user's VS Code server, where Puddle puts its guard settings.
    const MACHINE: &str = "/home/agent/.vscode-server/data/Machine/settings.json";

    /// How the canary log shows an ask with the scopes of the built-in GitHub extension's git
    /// credential provider (Copilot asks for the same scopes in another order).
    const GIT_ASK: &str = "getSessions repo,workflow,user:email,read:user";

    /// The guest user's VS Code server folder, as a shell variable `S`.
    const SERVER: &str =
        "S=$(ls -d /home/agent/.vscode-server/cli/servers/Stable-*/server | tail -n 1)";

    /// What the git extension's askpass would be asked in a terminal of the window, asked from outside
    /// it as the guest's root can: through the extension's own socket, with the extension's own
    /// helper, on the newest socket (a replaced server leaves its old one). It prints what `git
    /// credential fill` ends with (a `password=` line when a token came), and waits at most 30 s:
    /// with the GitHub sign-in off or absent the extension asks the person in the window instead.
    const ASKPASS: &str = r#"S=$(ls -d /home/agent/.vscode-server/cli/servers/Stable-*/server | tail -n 1)
H=$(ls -t /tmp/vscode-git-*.sock 2>/dev/null | head -n 1)
[ -n "$H" ] || { echo no-git-socket; exit 0; }
printf 'protocol=https\nhost=github.com\n\n' | timeout 30 env VSCODE_GIT_IPC_HANDLE=$H GIT_ASKPASS=$S/extensions/git/dist/askpass.sh VSCODE_GIT_ASKPASS_NODE=$S/node VSCODE_GIT_ASKPASS_MAIN=$S/extensions/git/dist/askpass-main.js git credential fill 2>&1 || true"#;

    /// Whether `git credential fill` ended with a password: a `password=` line with something after
    /// it (git prints the key empty when it gave up).
    fn has_password(said: &str) -> bool {
        said.lines().any(|l| {
            l.strip_prefix("password=")
                .is_some_and(|rest| !rest.is_empty())
        })
    }

    /// Copies a folder of logs into the artifact folder.
    fn copy_tree(from: &Path, to: &Path) {
        let Ok(entries) = std::fs::read_dir(from) else {
            return;
        };
        std::fs::create_dir_all(to).unwrap();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                copy_tree(&path, &to.join(entry.file_name()));
            } else {
                let _ = std::fs::copy(&path, to.join(entry.file_name()));
            }
        }
    }

    /// The text of the dialogs, input boxes and notifications the window shows, for a failure message.
    async fn window_shows(window: &mut VsCodeWindow) -> String {
        match window
            .evaluate(
                "[...document.querySelectorAll('.monaco-dialog-box, .quick-input-widget, .notification-toast')].filter(e => e.offsetParent !== null).map(e => [e.innerText, ...[...e.querySelectorAll('input')].map(i => i.placeholder)].join(' ')).join(' | ')",
            )
            .await
        {
            Ok(shown) => shown.as_str().unwrap_or_default().to_owned(),
            Err(error) => format!("(unreadable: {error})"),
        }
    }

    fn ssh_exe() -> PathBuf {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot is set");
        let ssh = PathBuf::from(root)
            .join("System32")
            .join("OpenSSH")
            .join("ssh.exe");
        assert!(ssh.is_file(), "{} is not installed", ssh.display());
        ssh
    }

    /// A stand-in for VS Code's `code.cmd` that writes its first two arguments one per line to `seen`:
    /// what the product's attach asked to open.
    fn stub_code(dir: &Path, seen: &Path) -> PathBuf {
        let bin = dir.join("code.cmd");
        std::fs::write(
            &bin,
            format!(
                "@echo off\r\n>\"{0}\" echo %1\r\n>>\"{0}\" echo %2\r\n",
                seen.display()
            ),
        )
        .unwrap();
        bin
    }

    pub(super) fn attach_story() {
        // One large future, and a Windows main thread has 1 MiB of stack: a thread with room.
        std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(run());
            })
            .unwrap()
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    }

    /// Where the pictures and logs go: `PUDDLE_VSCODE_OUT` (the job keeps it), else the scratch folder.
    fn out_dir(scratch: &Path) -> PathBuf {
        let dir = std::env::var_os("PUDDLE_VSCODE_OUT")
            .map_or_else(|| scratch.join("out"), PathBuf::from);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn shell(runtime: &MsbRuntime, sandbox: &WorkspaceId, script: &str) -> String {
        guest(runtime, sandbox, script).await
    }

    /// The server's own processes, for the record of what a reload replaced.
    const SERVER_PIDS: &str = "ps -eo pid,args | grep -E 'server-main|node.*vscode-server' | grep -v grep | awk '{print $1}' | tr '\\n' ','";

    /// U7c: a guest that patches its git extension makes the window open a local terminal and run a
    /// command on this computer. The patch is undone at the end, always.
    async fn hostile_code_runs_here(
        window: &mut VsCodeWindow,
        runtime: &MsbRuntime,
        sandbox: &WorkspaceId,
        probe: &Path,
        out: &Path,
    ) -> String {
        // The patch is read when the git extension starts, so it must not be written while the window is still
        // starting after the reload before this one: the extension would load it at once, run the probe in a
        // local terminal that takes the focus, and close the palette the next reload is driven through. The
        // extension serves its socket once it runs; a socket file left by the server the reload replaced is
        // not listening, so the kernel's list of bound sockets is the witness.
        eventually_async("the git extension is running", SHOWS, || async {
            let said = shell(
                runtime,
                sandbox,
                "grep -o '/tmp/vscode-git-[^ ]*\\.sock' /proc/net/unix | head -n 1",
            )
            .await;
            (!said.trim().is_empty()).then_some(())
        })
        .await;
        let code = local_terminal_probe("/tmp/u7c-ran", &probe.display().to_string());
        shell(
            runtime,
            sandbox,
            &format!(
                "set -eu; {SERVER}; M=$S/extensions/git/dist/main.js; B=/root/git-main.js.bak\n\
                 [ -f $B ] && cp $B $M; cp $M $B; rm -f /tmp/u7c-ran /tmp/u7c-ran.err\n\
                 cat >> $M <<'PROBE_EOF'\n;{code}\nPROBE_EOF\n\
                 grep -c u7c-ran $M"
            ),
        )
        .await;
        let back = window
            .reload(UP, &out.join("u7c-reload.png"))
            .await
            .expect("the window comes back");
        assert!(!back.folders.is_empty(), "{back:?}");
        eventually_async("the patched extension ran in the guest", SHOWS, || async {
            let marker = shell(runtime, sandbox, "cat /tmp/u7c-ran 2>/dev/null || true").await;
            marker.trim().parse::<u64>().ok()
        })
        .await;
        let who = eventually_async("the command ran on this computer", SHOWS, || async {
            std::fs::read(probe).ok().filter(|b| !b.is_empty())
        })
        .await;
        let who: String = String::from_utf8_lossy(&who)
            .chars()
            .filter(|c| *c != '\0' && *c != '\u{feff}')
            .collect::<String>()
            .trim()
            .to_owned();
        shell(
            runtime,
            sandbox,
            &format!(
                "{SERVER}; B=/root/git-main.js.bak\n\
                 [ -f $B ] && cp $B $S/extensions/git/dist/main.js && rm -f $B\n\
                 rm -f /tmp/u7c-ran /tmp/u7c-ran.err"
            ),
        )
        .await;
        std::fs::remove_file(probe).unwrap();
        who
    }

    /// The one booted VM and everything the phases share.
    struct Story {
        release: VsCodeRelease,
        modules: PathBuf,
        node: Node,
        scratch: tempfile::TempDir,
        out: PathBuf,
        // Kept alive for the whole story.
        _host: Host<MsbRuntime>,
        runtime: MsbRuntime,
        api: Api,
        events: Events,
        name: String,
        name_static: &'static str,
        sandbox: WorkspaceId,
        root: PathBuf,
        alias: String,
        args: Vec<String>,
        canary_log: PathBuf,
        probe: PathBuf,
    }

    impl Story {
        fn launch(&self, profile: Option<&str>, canary: bool) -> VsCodeLaunch {
            VsCodeLaunch {
                user_data: self.scratch.path().join("vsc-user"),
                extensions: self.scratch.path().join("vsc-ext"),
                ssh_config: self.root.join("ssh").join("config"),
                ssh: ssh_exe(),
                alias: self.alias.clone(),
                use_local_server: None,
                port: PORT,
                open: self.args.clone(),
                dev_extension: canary.then(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests")
                        .join("fixtures")
                        .join("canary-github-auth")
                }),
                profile: profile.map(str::to_owned),
                env: if canary {
                    vec![
                        ("PUDDLE_CANARY_TOKEN".to_owned(), CANARY.to_owned()),
                        (
                            "PUDDLE_CANARY_LOG".to_owned(),
                            self.canary_log.display().to_string(),
                        ),
                    ]
                } else {
                    Vec::new()
                },
            }
        }

        async fn shell(&self, script: &str) -> String {
            shell(&self.runtime, &self.sandbox, script).await
        }

        async fn alive(&self, pid: u32) -> bool {
            let script = format!("kill -0 {pid} 2>/dev/null && echo alive || echo dead");
            self.shell(&script).await.trim() == "alive"
        }

        async fn wait_for_git_socket(&self, script: &'static str) {
            eventually_async("the git extension's socket is up", SHOWS, || async {
                let said = self.shell(script).await;
                (!said.trim().is_empty()).then_some(())
            })
            .await;
        }
    }

    async fn run() {
        let mut story = boot().await;
        let mut window = phase_attach(&story).await;
        phase_guard_on(&story, &mut window).await;
        phase_trusted_mode(&story, &mut window).await;
        phase_reload_ends_terminal(&story, &mut window).await;
        let who = phase_hostile_code(&story, window).await;
        phase_profile_without_sign_in(&story, &who).await;
        story
            .api
            .post(&format!("/api/workspaces/{}/stop", story.name), "")
            .await;
        story
            .events
            .until(ended(story.name_static), Duration::from_secs(120))
            .await;
    }

    async fn boot() -> Story {
        let release = VsCodeRelease::from_env().expect("PUDDLE_VSCODE_DIR: ci/fetch-vscode.sh");
        let modules = PathBuf::from(
            std::env::var_os(BROWSER_MODULES_VAR)
                .expect("PUDDLE_VM_BROWSER_MODULES: ui/node_modules after npm ci --ignore-scripts"),
        );
        let node = Node::from_env();
        let scratch = tempfile::tempdir().unwrap();
        let out = out_dir(scratch.path());
        let seen = scratch.path().join("code-args");
        let bin = stub_code(scratch.path(), &seen);
        let mut options: HostOptions = named_id_options();
        options.launcher = Arc::new(VsCodeLauncher::with_program(&bin));
        let exe = std::env::current_exe().unwrap();
        let user_config = scratch.path().join("home/.ssh/config");
        let VmHost {
            host,
            runtime,
            settings,
            root,
            ..
        } = start_vm_host_config("vscode", options, move |config| {
            config.ssh.bridge_program = Some(exe);
            config.ssh.user_config = Some(user_config);
        })
        .await;
        let name = format!("{}-vscode", settings.prefix.as_str());
        let sandbox = WorkspaceId::new(&name).unwrap();
        let api = Api::new(host.url(), host.token());
        let mut events = api.events().await;
        let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
        let reply = api
            .post(
                "/api/workspaces",
                &json!({"name": name, "repositories": [], "memory_mib": 2048}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events
            .until(ended(name_static), Duration::from_secs(600))
            .await;
        assert_eq!(end["step"], "done", "{end}");
        // Two checkouts in the volume root: the attach finds them and writes the multi-root file.
        shell(
            &runtime,
            &sandbox,
            &format!(
                "cd /workspaces/{name} && git init -q api && git init -q web && chown -R agent: api web 2>/dev/null; true"
            ),
        )
        .await;
        let reply = api
            .put(
                &format!("/api/settings/workspaces/{name}"),
                &json!({"overrides": {"direct_ssh": true}}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let attach = api
            .post(
                &format!("/api/workspaces/{name}/attach"),
                &json!({"mode": "desktop"}).to_string(),
            )
            .await;
        assert_eq!(attach.json()["opened"], true, "{}", attach.body);
        let args: Vec<String> = std::fs::read_to_string(&seen)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let alias = format!("puddle-{name}");
        assert_eq!(args[0], "--file-uri", "{args:?}");
        assert!(
            args[1].starts_with(&format!("vscode-remote://ssh-remote+{alias}/workspaces/"))
                && args[1].ends_with(".code-workspace"),
            "{args:?}"
        );
        let canary_log = out.join("canary-asked.log");
        let probe = scratch.path().join("u7c-probe.txt");
        Story {
            release,
            modules,
            node,
            scratch,
            out,
            _host: host,
            runtime,
            api,
            events,
            name,
            name_static,
            sandbox,
            root,
            alias,
            args,
            canary_log,
            probe,
        }
    }

    /// The window attaches, and the explorer shows one root per checkout.
    async fn phase_attach(s: &Story) -> VsCodeWindow {
        let first = s.launch(None, true);
        first
            .prepare(&s.release)
            .await
            .expect("Remote-SSH installs");
        let mut window = first
            .start(&s.release, &s.node, &s.modules, &s.out.join("code.log"))
            .await
            .expect("VS Code starts");
        let up = window.attached(UP).await.expect("attached");
        window
            .screenshot(&s.out.join("attached.png"))
            .await
            .unwrap();
        assert!(up.remote.contains("SSH:"), "{up:?}");
        for folder in ["api", "web"] {
            assert!(
                up.folders.iter().any(|f| f == folder),
                "the explorer has no root {folder}: {up:?}"
            );
        }
        assert!(up.title.contains("(Workspace)"), "{up:?}");

        s.wait_for_git_socket("ls /tmp/vscode-git-*.sock 2>/dev/null | head -n 1")
            .await;
        window
    }

    /// U7b, the guard: Puddle's Machine settings turn the git extension's GitHub sign-in off, so the
    /// guest's askpass gets no token. The extension asks the person in the window instead.
    async fn phase_guard_on(s: &Story, window: &mut VsCodeWindow) {
        let guarded = s.shell(ASKPASS).await;
        let asked = window.dismiss_prompt(SHOWS).await.expect("the input box");
        assert!(
            !has_password(&guarded),
            "a token reached the guest with the guard setting on: {guarded}"
        );
        assert!(
            asked
                .as_deref()
                .is_some_and(|a| a.contains("Git: https://github.com")),
            "the git extension did not ask the person for github.com: {asked:?}; the window shows {}",
            window_shows(window).await
        );
        assert!(
            !std::fs::read_to_string(&s.canary_log)
                .unwrap()
                .contains(GIT_ASK),
            "the window's provider was asked for the git extension's token with the guard on"
        );
    }

    /// U7b, the trusted mode: the guard is only an accident guard. The guest reads the setting, so
    /// its root can turn it back on, and the next ask gets the token of the window's `github`
    /// provider: the canary here, a signed-in person's real token in use.
    async fn phase_trusted_mode(s: &Story, window: &mut VsCodeWindow) {
        let flipped = s
            .shell(&format!(
                "sed -i -E 's/(\"github\\.gitAuthentication\"[[:space:]]*:[[:space:]]*)false/\\1true/' {MACHINE} && grep -c '\"github.gitAuthentication\": true' {MACHINE}"
            ))
            .await;
        assert_eq!(
            flipped.trim(),
            "1",
            "the guest could not turn the setting on: {flipped}"
        );
        let mut token = String::new();
        let until = std::time::Instant::now() + SHOWS;
        while std::time::Instant::now() < until {
            token = s.shell(ASKPASS).await;
            if has_password(&token) {
                break;
            }
            // The setting had not reached the extension yet, and it asked the person: answer with nothing.
            window
                .dismiss_prompt(Duration::from_secs(2))
                .await
                .expect("the input box");
        }
        assert!(
            token.lines().any(|l| l == format!("password={CANARY}")),
            "the guest's askpass did not get the canary: {token}; the window shows {}",
            window_shows(window).await
        );
        assert!(
            std::fs::read_to_string(&s.canary_log)
                .unwrap()
                .contains(GIT_ASK),
            "the canary came without the window's provider being asked for the git extension's scopes"
        );
        // The window's own logs go into the artifact: they say why a window never attached or asked.
        copy_tree(
            &s.scratch.path().join("vsc-user").join("logs"),
            &s.out.join("vsc-logs"),
        );
    }

    /// U8: Reload Window ends a terminal, because the reload replaces the guest's server.
    async fn phase_reload_ends_terminal(s: &Story, window: &mut VsCodeWindow) {
        let opened = window
            .terminal("echo $$ > /tmp/u8.pid; exec sleep 100000")
            .await;
        if opened.is_err() {
            window.screenshot(&s.out.join("u8.png")).await.unwrap();
        }
        assert!(
            opened.is_ok(),
            "no terminal opened: {opened:?}; the window shows {}",
            window_shows(window).await
        );
        let pid = eventually_async(
            "the terminal ran its command in the guest",
            SHOWS,
            || async {
                s.shell("cat /tmp/u8.pid 2>/dev/null || true")
                    .await
                    .trim()
                    .parse::<u32>()
                    .ok()
            },
        )
        .await;
        assert!(
            s.alive(pid).await,
            "the terminal's process {pid} is not running before the reload"
        );
        let before = s.shell(SERVER_PIDS).await;
        window
            .reload(UP, &s.out.join("u8-reload.png"))
            .await
            .expect("the window comes back");
        let after = s.shell(SERVER_PIDS).await;
        assert!(
            !s.alive(pid).await,
            "the terminal's process {pid} survived the reload (servers before [{before}], after [{after}]): with the default settings a reload used to replace the server and end it; the notes on Reload Window need the new fact"
        );
    }

    /// U7c: guest code makes the window run a command on this computer (the documented trust).
    /// Closes the window and returns who ran it.
    async fn phase_hostile_code(s: &Story, mut window: VsCodeWindow) -> String {
        let who =
            hostile_code_runs_here(&mut window, &s.runtime, &s.sandbox, &s.probe, &s.out).await;
        window.screenshot(&s.out.join("u7c.png")).await.unwrap();
        assert!(!who.is_empty(), "the probe file is empty");
        window.close().await.expect("the window closes");
        who
    }

    /// U7d: in a profile with the GitHub sign-in off and nothing in its place, the guest gets no
    /// token, with the guard setting still turned on by the guest above: the profile alone keeps it
    /// out. Its code still runs here.
    async fn phase_profile_without_sign_in(s: &Story, who: &str) {
        let second = s.launch(Some("puddle-test"), false);
        // A profile starts empty and exists only once a window has opened it: open it, close it, then
        // give it Remote-SSH, which it does not inherit.
        let first_open = VsCodeLaunch {
            open: Vec::new(),
            ..second.clone()
        }
        .start(
            &s.release,
            &s.node,
            &s.modules,
            &s.out.join("code-profile-new.log"),
        )
        .await
        .expect("VS Code makes the profile");
        first_open
            .close()
            .await
            .expect("the profile's first window closes");
        second
            .prepare_profile(&s.release)
            .await
            .expect("Remote-SSH installs in the profile");
        let mut window = second
            .start(
                &s.release,
                &s.node,
                &s.modules,
                &s.out.join("code-profile.log"),
            )
            .await
            .expect("VS Code starts in the profile");
        window.attached(UP).await.expect("attached again");
        s.wait_for_git_socket("ls /tmp/vscode-git-*.sock 2>/dev/null | head -n 1 || true")
            .await;
        // The first window's server may have left its socket behind, so ask until the live extension
        // answers by asking the person.
        let mut asked = None;
        let mut said = String::new();
        let until = std::time::Instant::now() + SHOWS;
        while asked.is_none() && std::time::Instant::now() < until {
            said = s.shell(ASKPASS).await;
            assert!(
                !said.contains("gho_") && !has_password(&said),
                "a token reached the guest with no GitHub provider on this side: {said}"
            );
            asked = window
                .dismiss_prompt(Duration::from_secs(2))
                .await
                .expect("the input box");
        }
        assert!(
            asked
                .as_deref()
                .is_some_and(|a| a.contains("Git: https://github.com")),
            "the git extension in the profile's window never asked the person for github.com: {asked:?}, askpass said {said}"
        );
        let who_again =
            hostile_code_runs_here(&mut window, &s.runtime, &s.sandbox, &s.probe, &s.out).await;
        assert_eq!(
            who_again, who,
            "the same user runs the command in the profile"
        );
        window.screenshot(&s.out.join("u7d.png")).await.unwrap();
        window.close().await.expect("the window closes");
    }
}
