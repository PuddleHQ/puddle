// SPDX-License-Identifier: AGPL-3.0-or-later
//! A stand-in for `gh` and `git`, for the source tests.
//!
//! The tests copy this binary to a temporary directory under the name of the real tool and write
//! `<binary path>.behaviour` next to it. The behaviour file is a list of sections:
//!
//! ```text
//! [credential fill]      <- used when the arguments contain this text; `[]` matches always
//! exit=0
//! sleep_ms=0
//! echo_stdin=1           <- print the standard input first (bytes, as read)
//! stdout=password=x\n    <- `\n` is a newline
//! stderr=code\n         <- printed (and flushed) before the sleep, as `gh` prints its prompt
//! run=/usr/bin/git       <- instead: run this program with the same arguments and standard input,
//! env=NAME=value            with these variables added, and answer what it answered
//! gcm_store=/path/file   <- instead: answer as Git Credential Manager's GitHub provider over the
//! gcm_sign_in=user:token    Windows credential store, kept in that file (see [`gcm`])
//! ```
//!
//! Every call appends the arguments, the standard input and the prompt-related environment to
//! `<binary path>.log`, so a test can check what Puddle sent.
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::expect_used,
    reason = "a command-line stand-in answers on standard output"
)]

use std::io::{Read, Write};

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n")
}

fn main() {
    let exe = std::env::current_exe().expect("own path");
    let behaviour =
        std::fs::read_to_string(format!("{}.behaviour", exe.display())).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let joined = args.join(" ");
    let mut stdin = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut stdin);
    let stdin_text = String::from_utf8_lossy(&stdin).into_owned();

    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{}.log", exe.display()))
        .expect("log");
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "<unset>".to_owned());
    let _ = writeln!(
        log,
        "ARGS {joined}\nSTDIN {}\nENV GCM_INTERACTIVE={} GIT_TERMINAL_PROMPT={} GIT_ASKPASS={} GH_TOKEN={} GH_PROMPT_DISABLED={}",
        stdin_text.replace('\n', "\\n"),
        env("GCM_INTERACTIVE"),
        env("GIT_TERMINAL_PROMPT"),
        env("GIT_ASKPASS"),
        env("GH_TOKEN"),
        env("GH_PROMPT_DISABLED"),
    );

    let (mut exit, mut sleep_ms, mut echo) = (0, 0, false);
    let (mut stdout, mut stderr) = (String::new(), String::new());
    let (mut run, mut run_env) = (None, Vec::new());
    let (mut gcm_store, mut gcm_sign_in) = (None, None);
    let mut active = false;
    for line in behaviour.lines() {
        if let Some(rest) = line.strip_prefix('[') {
            let needle = rest.trim_end_matches(']');
            active = needle.is_empty() || joined.contains(needle);
            if active {
                (exit, sleep_ms, echo) = (0, 0, false);
                (stdout, stderr) = (String::new(), String::new());
                (run, run_env) = (None, Vec::new());
                (gcm_store, gcm_sign_in) = (None, None);
            }
        } else if active {
            match line.split_once('=') {
                Some(("exit", v)) => exit = v.parse().unwrap_or(1),
                Some(("sleep_ms", v)) => sleep_ms = v.parse().unwrap_or(0),
                Some(("echo_stdin", v)) => echo = v == "1",
                Some(("stdout", v)) => stdout = unescape(v),
                Some(("stderr", v)) => stderr = unescape(v),
                Some(("run", v)) => run = Some(v.to_owned()),
                Some(("gcm_store", v)) => gcm_store = Some(v.to_owned()),
                Some(("gcm_sign_in", v)) => gcm_sign_in = Some(v.to_owned()),
                Some(("env", v)) => {
                    run_env.extend(v.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())));
                }
                _ => {}
            }
        }
    }
    if let Some(program) = run {
        leave(pass_to(&program, &args, &run_env, &stdin));
    }
    if let Some(store) = gcm_store {
        let verb = args.last().map_or("", String::as_str);
        leave(gcm(verb, &stdin_text, &store, gcm_sign_in.as_deref()));
    }
    eprint!("{stderr}");
    let _ = std::io::stderr().flush();
    if sleep_ms > 0 {
        // A tool that outlives the window under test.
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }
    if echo {
        let _ = std::io::stdout().write_all(&stdin);
    }
    print!("{stdout}");
    let _ = std::io::stdout().flush();
    leave(exit);
}

/// Git Credential Manager's GitHub provider over the Windows credential store, as a Git credential
/// helper (`get`, `store`, `erase`), as far as finding and keeping an entry goes. Modelled on its
/// sources (`src/GitHub/GitHubHostProvider.cs` and `src/Core/Interop/Windows/WindowsCredentialManager.cs`
/// of git-ecosystem/git-credential-manager):
///
/// - the service is the request's URL without the user: `https://github.com`, or
///   `https://github.com/<path>` when Git sent a path;
/// - an entry matches when its user is the account asked for (any, when none was) and its target,
///   less `git:` and a user part, names the same scheme, host and path;
/// - without an account the one entry of the service is used (with several, the manager asks the
///   user to pick; this stand-in treats that as nothing found);
/// - nothing found: a sign-in, which fails when `GCM_INTERACTIVE` is `never`;
/// - `store` updates the entry `git:<service>` when it holds the same account, else writes it, or
///   `git:https://<account>@<host><path>` when it holds another account.
///
/// The store file has one entry per line: target, user and secret, separated by tabs.
fn gcm(verb: &str, request: &str, store: &str, sign_in: Option<&str>) -> i32 {
    let field = |key: &str| {
        request
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .filter(|v| !v.is_empty())
    };
    let (Some(protocol), Some(host)) = (field("protocol"), field("host")) else {
        return 1;
    };
    let path = field("path").map(|p| p.trim_matches('/'));
    let service = path.map_or_else(
        || format!("{protocol}://{host}"),
        |p| format!("{protocol}://{host}/{p}"),
    );
    let account = field("username");
    let text = std::fs::read_to_string(store).unwrap_or_default();
    let mut entries: Vec<[String; 3]> = text
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t').map(str::to_owned);
            Some([parts.next()?, parts.next()?, parts.next()?])
        })
        .collect();
    match verb {
        "get" => {
            let matching: Vec<&[String; 3]> = entries
                .iter()
                .filter(|[target, user, _]| {
                    account.is_none_or(|a| a == user) && gcm_target_matches(&service, target)
                })
                .collect();
            if let [[_, user, secret]] = matching.as_slice() {
                answer(user, secret);
                return 0;
            }
            if std::env::var("GCM_INTERACTIVE").is_ok_and(|v| v == "never") {
                eprintln!("fatal: Cannot prompt because user interactivity has been disabled.");
                return 1;
            }
            match sign_in.and_then(|s| s.split_once(':')) {
                Some((user, token)) => {
                    answer(user, token);
                    0
                }
                None => 1,
            }
        }
        "store" => {
            let (Some(user), Some(secret)) = (account, field("password")) else {
                return 0;
            };
            let plain = format!("git:{service}");
            let target = match entries.iter().find(|[t, _, _]| *t == plain) {
                Some([_, held, _]) if held != user => {
                    let tail = path.map_or_else(String::new, |p| format!("/{p}"));
                    format!("git:{protocol}://{user}@{host}{tail}")
                }
                _ => plain,
            };
            entries.retain(|[t, _, _]| *t != target);
            entries.push([target, user.to_owned(), secret.to_owned()]);
            let mut lines = String::new();
            for [t, u, s] in &entries {
                for (part, end) in [(t, '\t'), (u, '\t'), (s, '\n')] {
                    lines.push_str(part);
                    lines.push(end);
                }
            }
            std::fs::write(store, lines).expect("the store file");
            0
        }
        _ => 0,
    }
}

/// A credential helper's answer to `get`: the protocol's output on standard output, written as
/// bytes rather than through `println!`, which is for messages to a person.
fn answer(user: &str, password: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(format!("username={user}\npassword={password}\n").as_bytes());
    let _ = out.flush();
}

/// Whether a store target (`git:https://[user@]host[/path]`) names the service's scheme, host and
/// path; the path is `/` when there is none, and compared without regard to case.
fn gcm_target_matches(service: &str, target: &str) -> bool {
    let Some(target) = target.strip_prefix("git:") else {
        return false;
    };
    if target == service {
        return true;
    }
    let split = |url: &str| -> Option<(String, String, String)> {
        let (scheme, rest) = url.split_once("://")?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        Some((
            scheme.to_ascii_lowercase(),
            host.to_ascii_lowercase(),
            format!("/{}", path.trim_end_matches('/')).to_ascii_lowercase(),
        ))
    };
    split(service).is_some_and(|s| split(target).is_some_and(|t| s == t))
}

/// Runs the real `program` as this call, and returns its exit code.
fn pass_to(program: &str, args: &[String], env: &[(String, String)], stdin: &[u8]) -> i32 {
    let mut child = std::process::Command::new(program)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("the program to pass the call to");
    if let Some(mut input) = child.stdin.take() {
        let _ = input.write_all(stdin);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

/// Ends the process without the exit-time profile write of an instrumented build.
///
/// The tests kill this tool on a timeout (`kill_on_drop`), and a kill during that write leaves a
/// short `.profraw` file that fails the whole coverage merge. The file is under `tests/`, which
/// the coverage report leaves out, so its profile is never read anyway.
fn leave(code: i32) -> ! {
    #[cfg(unix)]
    {
        // Declared here because a `[[bin]]` build doesn't link the dev-dependencies, and `libc`
        // isn't a dependency of this crate.
        #[expect(
            unsafe_code,
            reason = "a one-function C declaration; `_exit` has no preconditions"
        )]
        unsafe extern "C" {
            safe fn _exit(code: i32) -> !;
        }
        _exit(code)
    }
    #[cfg(not(unix))]
    #[expect(
        clippy::exit,
        reason = "no profile to skip off unix; a stand-in exits with its code"
    )]
    std::process::exit(code)
}
