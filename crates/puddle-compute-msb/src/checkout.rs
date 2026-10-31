// SPDX-License-Identifier: GPL-3.0-or-later
//! The one mount source that is not under the guest share: the `.git` directory of a checkout the
//! user picked ([`puddle_compute::MountKind::Checkout`]).
//!
//! The rule is narrow on purpose. The source must be the canonical path of a real `.git`
//! directory, with a git directory's structure, outside every folder of Puddle's own and (on
//! Unix) owned by the user Puddle runs as. [`puddle_compute::SandboxSpec::validate`] already
//! keeps it out of any sandbox with a route, and [`crate::spec::builder`] passes it to msb
//! read-only like every other file mount, so msb's file server refuses every write whatever the
//! guest does to its own mount flags.
//!
//! What it does not cover, by design: the checks are by path, so a program of the same user that
//! swaps the folder between the check and msb's open (narrowed by a second check right before the
//! create) or puts a hard link or a bind mount inside `.git` gets past them. Such a program
//! already has the user's files; nothing of the checkout runs on the host either way.

#[cfg(unix)]
use std::fs::Metadata;
use std::path::Path;

use puddle_compute::ComputeError;

/// Checks `git_dir` against the checkout rule; `protected` are the folders no checkout may lie
/// in or hold (the msb home, the guest share, Puddle's data folder).
pub(crate) fn check(git_dir: &Path, protected: &[&Path]) -> Result<(), ComputeError> {
    #[cfg(unix)]
    let owner = Some(rustix::process::geteuid().as_raw());
    // On Windows the owner is not compared: a file that a member of Administrators makes is often
    // owned by that group, not by the user (git's own `safe.directory` check has to allow for it),
    // so the check would refuse ordinary checkouts. Nothing of the checkout runs on the host.
    #[cfg(not(unix))]
    let owner = None;
    check_as(git_dir, protected, owner)
}

/// [`check`] with the owner a checkout must have on Unix (`None`: not compared).
fn check_as(git_dir: &Path, protected: &[&Path], owner: Option<u32>) -> Result<(), ComputeError> {
    let refuse = |why: String| ComputeError::InvalidSpec {
        reason: format!("the checkout {} {why}", git_dir.display()),
    };
    if !git_dir.is_absolute() {
        return Err(refuse("is not an absolute path".into()));
    }
    if !on_local_drive(git_dir) {
        return Err(refuse("is on a network share, not a local drive".into()));
    }
    let canonical =
        dunce::canonicalize(git_dir).map_err(|e| refuse(format!("can't be resolved: {e}")))?;
    if canonical.as_os_str() != git_dir.as_os_str() {
        return Err(refuse(format!(
            "is not written as its canonical path {}: give it with no `..` and no link, junction \
             or other spelling of a folder in it",
            canonical.display()
        )));
    }
    puddle_compute::check_checkout_dir(git_dir).map_err(refuse)?;
    #[cfg(unix)]
    let meta =
        std::fs::symlink_metadata(git_dir).map_err(|e| refuse(format!("can't be read: {e}")))?;
    for root in protected {
        let root = dunce::canonicalize(root).map_err(|e| {
            refuse(format!(
                "can't be checked against Puddle's folder {}: {e}",
                root.display()
            ))
        })?;
        if within(&canonical, &root) {
            return Err(refuse(format!(
                "lies inside Puddle's folder {}",
                root.display()
            )));
        }
        if within(&root, &canonical) {
            return Err(refuse(format!("holds Puddle's folder {}", root.display())));
        }
    }
    match owner {
        #[cfg(unix)]
        Some(uid) => owned_by(&meta, uid).map_err(refuse),
        _ => Ok(()),
    }
}

/// Whether `path` starts on a local drive: always on Unix; on Windows a drive letter, never a
/// network share (`\\server\share`, also what a mapped network drive resolves to) or a device
/// path.
fn on_local_drive(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(p)) => matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
        _ => true,
    }
}

/// Whether `path` is `root` or lies below it. On Windows and macOS, whose usual file systems
/// ignore case, the components are compared without case, so another spelling of a protected
/// folder is still that folder.
fn within(path: &Path, root: &Path) -> bool {
    within_as(path, root, cfg!(any(windows, target_os = "macos")))
}

/// [`within`], comparing without case when `fold`.
fn within_as(path: &Path, root: &Path, fold: bool) -> bool {
    if fold {
        let folded = |p: &Path| -> Vec<String> {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect()
        };
        folded(path).starts_with(&folded(root))
    } else {
        path.starts_with(root)
    }
}

/// On Unix the checkout must belong to the user Puddle runs as, as git itself asks of a
/// repository before it reads its config (`safe.directory`).
#[cfg(unix)]
fn owned_by(meta: &Metadata, uid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if meta.uid() == uid {
        Ok(())
    } else {
        Err(format!(
            "belongs to another user (uid {}, Puddle runs as {uid})",
            meta.uid()
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A temporary folder that holds a checkout and Puddle's folders, removed at the end.
    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::Builder::new()
                    .prefix("puddle-checkout-rule-")
                    .tempdir()
                    .unwrap(),
            }
        }

        /// The canonical base folder (the temp folder may sit behind a link, as on macOS).
        fn base(&self) -> PathBuf {
            dunce::canonicalize(self.dir.path()).unwrap()
        }

        /// A git directory by structure at `<base>/<rel>/.git`; returns its path.
        fn repo(&self, rel: &str) -> PathBuf {
            // Joined part by part: a `/` inside one `join` stays a `/` on Windows, and the rule
            // refuses a path that is not written the way the OS spells it.
            let mut git = self.base();
            git.extend(rel.split('/'));
            git.push(".git");
            make_git_dir(&git);
            git
        }

        fn protected(&self) -> Vec<PathBuf> {
            let data = self.base().join("data");
            let share = data.join("guest-share");
            let home = data.join("msb");
            for d in [&share, &home] {
                std::fs::create_dir_all(d).unwrap();
            }
            vec![home, share, data]
        }

        fn check(&self, git: &Path) -> Result<(), String> {
            let protected = self.protected();
            let roots: Vec<&Path> = protected.iter().map(PathBuf::as_path).collect();
            check(git, &roots).map_err(|e| e.to_string())
        }
    }

    fn make_git_dir(git: &Path) {
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::create_dir_all(git.join("refs").join("heads")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(git.join("config"), "[core]\n\tbare = false\n").unwrap();
    }

    fn refused(result: Result<(), String>, want: &str) {
        let err = result.expect_err("the checkout was accepted");
        assert!(err.contains(want), "want {want:?} in {err:?}");
    }

    #[test]
    fn the_canonical_git_directory_of_a_checkout_is_accepted() {
        let f = Fixture::new();
        let git = f.repo("api");
        f.check(&git).unwrap();
    }

    #[test]
    fn a_relative_path_is_refused() {
        let f = Fixture::new();
        refused(f.check(Path::new("api/.git")), "is not an absolute path");
    }

    #[test]
    fn a_path_that_climbs_with_dot_dot_is_refused_even_when_it_lands_on_a_checkout() {
        let f = Fixture::new();
        f.repo("api");
        std::fs::create_dir_all(f.base().join("other")).unwrap();
        let climb = f.base().join("other").join("..").join("api").join(".git");
        refused(f.check(&climb), "canonical path");
    }

    #[test]
    fn a_missing_path_is_refused() {
        let f = Fixture::new();
        refused(
            f.check(&f.base().join("gone").join(".git")),
            "can't be resolved",
        );
    }

    #[test]
    fn a_folder_that_is_not_named_dot_git_is_refused_so_a_bare_repository_is_too() {
        let f = Fixture::new();
        let bare = f.base().join("api.git");
        make_git_dir(&bare);
        refused(f.check(&bare), "is not a `.git` directory");
        refused(f.check(&f.base()), "is not a `.git` directory");
    }

    #[test]
    fn a_gitdir_file_of_a_worktree_or_submodule_is_refused_with_the_reason() {
        let f = Fixture::new();
        let main = f.repo("api");
        let linked = f.base().join("api-feature");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", main.join("worktrees").join("f").display()),
        )
        .unwrap();
        refused(
            f.check(&linked.join(".git")),
            "linked worktree or a submodule",
        );
    }

    #[test]
    fn a_dot_git_without_the_structure_of_a_git_directory_is_refused() {
        let f = Fixture::new();
        for (missing, want) in [
            ("HEAD", "no HEAD file"),
            ("objects", "no objects directory"),
            ("refs", "no refs directory"),
        ] {
            let git = f.repo(&format!("no-{missing}"));
            let path = git.join(missing);
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            refused(f.check(&git), want);
        }
        // An entry of the wrong kind counts as missing.
        let git = f.repo("head-dir");
        std::fs::remove_file(git.join("HEAD")).unwrap();
        std::fs::create_dir(git.join("HEAD")).unwrap();
        refused(f.check(&git), "no HEAD file");
        let empty = f.base().join("empty").join(".git");
        std::fs::create_dir_all(&empty).unwrap();
        refused(f.check(&empty), "is not a git directory");
    }

    #[test]
    fn a_linked_worktrees_git_directory_is_refused() {
        let f = Fixture::new();
        let git = f.repo("wt");
        std::fs::write(git.join("commondir"), "../..\n").unwrap();
        refused(f.check(&git), "commondir");
    }

    #[test]
    fn a_checkout_inside_any_of_puddles_folders_is_refused() {
        let f = Fixture::new();
        for rel in [
            "data/guest-share/api",
            "data/msb/volumes/api",
            "data/settings/api",
        ] {
            let git = f.repo(rel);
            refused(f.check(&git), "lies inside Puddle's folder");
        }
    }

    #[test]
    fn a_checkout_that_holds_one_of_puddles_folders_is_refused() {
        let f = Fixture::new();
        let git = f.repo("home");
        let protected = [git.join("puddle-data")];
        std::fs::create_dir_all(&protected[0]).unwrap();
        let err = check(&git, &[protected[0].as_path()]).unwrap_err();
        assert!(err.to_string().contains("holds Puddle's folder"), "{err}");
    }

    #[test]
    fn a_protected_folder_that_cannot_be_resolved_refuses_rather_than_skips() {
        let f = Fixture::new();
        let git = f.repo("api");
        let gone = f.base().join("no-such-data");
        let err = check(&git, &[gone.as_path()]).unwrap_err();
        assert!(err.to_string().contains("can't be checked"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_anywhere_in_the_path_is_refused() {
        let f = Fixture::new();
        let git = f.repo("real");
        // The `.git` itself is a link to a real git directory.
        let a = f.base().join("a");
        std::fs::create_dir_all(&a).unwrap();
        std::os::unix::fs::symlink(&git, a.join(".git")).unwrap();
        refused(f.check(&a.join(".git")), "canonical path");
        // A parent folder is a link.
        std::os::unix::fs::symlink(f.base().join("real"), f.base().join("b")).unwrap();
        refused(f.check(&f.base().join("b").join(".git")), "canonical path");
        // A link to one of Puddle's folders is refused for the same reason, before anything else.
        let protected = f.protected();
        let inside = protected[1].join("api").join(".git");
        make_git_dir(&inside);
        std::os::unix::fs::symlink(protected[1].join("api"), f.base().join("c")).unwrap();
        refused(f.check(&f.base().join("c").join(".git")), "canonical path");
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_head_objects_or_refs_entry_is_refused() {
        let f = Fixture::new();
        let elsewhere = f.repo("elsewhere");
        for entry in ["HEAD", "objects", "refs"] {
            let git = f.repo(&format!("linked-{entry}"));
            let path = git.join(entry);
            if path.is_dir() {
                std::fs::remove_dir_all(&path).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            std::os::unix::fs::symlink(elsewhere.join(entry), &path).unwrap();
            refused(f.check(&git), &format!("its {entry} is a link"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_checkout_of_another_user_is_refused_by_the_whole_rule() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        let git = f.repo("api");
        let protected = f.protected();
        let roots: Vec<&Path> = protected.iter().map(PathBuf::as_path).collect();
        let uid = std::fs::symlink_metadata(&git).unwrap().uid();
        check_as(&git, &roots, Some(uid)).unwrap();
        // Where the owner is not compared (Windows), another owner passes.
        check_as(&git, &roots, None).unwrap();
        // What `check` itself compares with: the user the tests run as owns what they made.
        check(&git, &roots).unwrap();
        let other = uid.wrapping_add(1);
        let err = check_as(&git, &roots, Some(other)).unwrap_err().to_string();
        assert!(err.contains("belongs to another user"), "{err}");
        assert!(err.contains(&format!("Puddle runs as {other}")), "{err}");
    }

    #[test]
    fn only_a_local_drive_counts() {
        assert!(on_local_drive(Path::new("/home/u/api/.git")));
        #[cfg(windows)]
        {
            assert!(on_local_drive(Path::new(r"C:\Users\u\api\.git")));
            assert!(on_local_drive(Path::new(r"\\?\C:\Users\u\api\.git")));
            for remote in [
                r"\\server\share\api\.git",
                r"\\?\UNC\server\share\api\.git",
                r"\\.\pipe\x\.git",
            ] {
                assert!(!on_local_drive(Path::new(remote)), "{remote}");
            }
        }
    }

    #[test]
    fn a_protected_folder_counts_in_any_case_where_the_file_system_ignores_case() {
        let root = Path::new("/Users/u/Library/Application Support/puddle");
        let other_case = Path::new("/Users/u/Library/application support/PUDDLE/x/.git");
        assert!(within(
            Path::new("/Users/u/Library/Application Support/puddle/x"),
            root
        ));
        assert_eq!(
            within(other_case, root),
            cfg!(any(windows, target_os = "macos"))
        );
        assert!(!within(
            Path::new("/Users/u/Library/Application Support/puddle2"),
            root
        ));
        assert!(within_as(other_case, root, true));
        assert!(!within_as(other_case, root, false));
        let neighbour = Path::new("/Users/u/Library/Application Support/puddle2");
        assert!(!within_as(neighbour, root, true));
    }
}
