// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn target(path: &str, file: bool) -> DesktopTarget {
    DesktopTarget {
        ssh_host: "puddle-web".into(),
        path: path.into(),
        is_workspace_file: file,
    }
}

#[test]
fn a_folder_and_a_multi_root_file_open_through_the_ssh_host() {
    assert_eq!(
        arguments(&target("/workspaces/abc/app", false)),
        [
            "--folder-uri",
            "vscode-remote://ssh-remote+puddle-web/workspaces/abc/app"
        ]
    );
    assert_eq!(
        arguments(&target(
            "/workspaces/abc/.puddle/editor/abc.code-workspace",
            true
        )),
        [
            "--file-uri",
            "vscode-remote://ssh-remote+puddle-web/workspaces/abc/.puddle/editor/abc.code-workspace"
        ]
    );
}

#[test]
fn a_path_is_percent_encoded_so_it_cannot_change_the_uri() {
    let [_, uri] = arguments(&target("/w/a b?x#y&z%", false));
    assert_eq!(
        uri,
        "vscode-remote://ssh-remote+puddle-web/w/a%20b%3Fx%23y%26z%25"
    );
}

#[test]
fn the_link_for_another_machine_has_the_same_encoded_place_as_the_command() {
    assert_eq!(
        target("/w/a b?x#y&z%", false).open_uri(),
        "vscode://vscode-remote/ssh-remote+puddle-web/w/a%20b%3Fx%23y%26z%25"
    );
}

#[test]
fn the_default_launcher_runs_the_code_command() {
    let launcher = VsCodeLauncher::default();
    let expected = if cfg!(windows) { "code.cmd" } else { "code" };
    assert_eq!(launcher.program, expected);
}

#[test]
fn stderr_is_one_short_line() {
    assert_eq!(one_line(""), "");
    assert_eq!(one_line("  a\n\tb \u{7} c "), ": a b c");
    assert_eq!(one_line(&"x".repeat(1000)).len(), 302);
}

#[cfg(unix)]
mod commands {
    use super::*;

    fn script(dir: &std::path::Path, body: &str) -> VsCodeLauncher {
        let path = dir.join("code");
        // Written by a child process: a write from this process can leave its descriptor in a
        // sibling test's forked child, and the exec below then fails with "Text file busy".
        puddle_test_kit::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        VsCodeLauncher::with_program(path).with_patience(Duration::from_millis(1500))
    }

    fn record() -> WorkspaceRecord {
        WorkspaceRecord::new(
            puddle_types::WorkspaceId::new("0123456789abcdef0123456789abcdef").unwrap(),
            puddle_api::DisplayName::parse("web").unwrap(),
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn the_command_gets_the_uri_and_its_success_is_success() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("args");
        let launcher = script(
            dir.path(),
            &format!("printf '%s\\n' \"$@\" > '{}'", out.display()),
        );
        launcher
            .open_desktop(&record(), &target("/workspaces/x", false))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(out).unwrap(),
            "--folder-uri\nvscode-remote://ssh-remote+puddle-web/workspaces/x\n"
        );
    }

    #[tokio::test]
    async fn a_command_that_fails_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = script(dir.path(), "echo 'no display' >&2; exit 3");
        let err = launcher
            .open_desktop(&record(), &target("/w", false))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exit status: 3") && err.contains("no display"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_command_that_keeps_running_counts_as_started() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = script(dir.path(), "exec sleep 1").with_patience(Duration::from_millis(100));
        launcher
            .open_desktop(&record(), &target("/w", false))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_missing_command_names_the_fix_and_the_host_to_open_by_hand() {
        let launcher = VsCodeLauncher::with_program("/nonexistent/code");
        let err = launcher
            .open_desktop(&record(), &target("/w", false))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not found") && err.contains("puddle-web"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_command_that_cannot_be_started_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("code");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        // Not executable.
        let launcher = VsCodeLauncher::with_program(path);
        let err = launcher
            .open_desktop(&record(), &target("/w", false))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("could not start VS Code"), "{err}");
    }
}

#[cfg(windows)]
mod batch_file {
    use super::*;

    /// A `code.cmd` that writes the arguments it got, one per line, to a file beside it.
    fn stub(dir: &std::path::Path) -> (VsCodeLauncher, std::path::PathBuf) {
        let seen = dir.join("seen.txt");
        let path = dir.join("code.cmd");
        std::fs::write(
            &path,
            format!(
                "@echo off\r\n>\"{0}\" echo %1\r\n>>\"{0}\" echo %2\r\n",
                seen.display()
            ),
        )
        .unwrap();
        (VsCodeLauncher::with_program(path), seen)
    }

    fn record() -> WorkspaceRecord {
        WorkspaceRecord::new(
            puddle_types::WorkspaceId::new("0123456789abcdef0123456789abcdef").unwrap(),
            puddle_api::DisplayName::parse("web").unwrap(),
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn a_batch_file_gets_the_uri_of_a_path_full_of_cmd_metacharacters_unchanged() {
        // A folder with a space in the program's own path too: `cmd` has to find the file.
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("Program Files");
        std::fs::create_dir(&spaced).unwrap();
        let (launcher, seen) = stub(&spaced);
        let path = "/workspaces/a b%PATH%&calc^x|y<z>(1)!q";
        launcher
            .open_desktop(&record(), &target(path, false))
            .await
            .unwrap();
        let [flag, uri] = arguments(&target(path, false));
        let got = std::fs::read_to_string(seen).unwrap();
        // `%1` keeps the quotes Rust puts round an argument `cmd` could misread; the value
        // inside them is the URI, byte for byte.
        let got: Vec<&str> = got.lines().map(|l| l.trim_matches('"')).collect();
        assert_eq!(got, [flag.as_str(), uri.as_str()]);
        assert!(
            !uri.contains(['&', '^', ' ', '|', '<', '>', '(', '!', '"']),
            "{uri}"
        );
    }

    #[tokio::test]
    async fn a_batch_file_that_fails_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("code.cmd");
        std::fs::write(&path, "@echo off\r\necho no display 1>&2\r\nexit /b 3\r\n").unwrap();
        let err = VsCodeLauncher::with_program(path)
            .open_desktop(&record(), &target("/w", false))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("exit code: 3") && err.contains("no display"),
            "{err}"
        );
    }
}
