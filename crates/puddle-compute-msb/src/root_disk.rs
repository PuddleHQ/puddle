// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's check of msb's root-disk journal before a sandbox starts.
//!
//! msb shares each sandbox's `runtime/` folder with its guest, writable, at `/.msb`, and keeps
//! `root-disk.json` there: the files that make up the root disk, as absolute host paths. At the
//! next start msb attaches the files that journal names as the writable root disk, and it does not
//! check where they are. A root process in the guest can therefore rewrite the journal to name
//! another sandbox's disk or any file of the host (upstream advisory GHSA-3f94-vm6x-3jv3, no fixed
//! release). Until msb confines the journal itself, the adapter reads it before every start and
//! refuses the start when a file it names lies outside the sandbox's own folder.
//!
//! The sandbox is stopped while this runs, so the guest cannot change the journal between the check
//! and msb's read of it.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use puddle_compute::ComputeError;
use serde::Deserialize;

/// The journal's file name in the sandbox's `runtime/` folder (msb's name for it).
const JOURNAL: &str = "root-disk.json";

/// msb refuses a larger journal itself.
const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;

/// The parts of msb's journal that name host files. msb's schema has more fields; they are not
/// paths, so the check ignores them.
#[derive(Deserialize)]
struct Journal {
    /// The configured base the chain started from. msb compares it with the sandbox's config
    /// and never opens it, but a guest that rewrites the layers sets it too, so it is checked.
    #[serde(default)]
    launch_base: Option<PathBuf>,
    /// The disk files, oldest first, the last one the writable head.
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    path: PathBuf,
}

/// Why a start is refused: the journal names a file outside the sandbox's folder, or it cannot be
/// read well enough to tell.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum JournalProblem {
    /// The journal itself is a link, which msb never writes.
    #[error("msb's root-disk journal {journal} is a link")]
    JournalIsLink {
        /// The journal.
        journal: PathBuf,
    },
    /// The journal could not be read or parsed.
    #[error("msb's root-disk journal {journal} could not be read: {reason}")]
    Unreadable {
        /// The journal.
        journal: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// A named file is not an absolute path.
    #[error("msb's root-disk journal names {path:?}, which is not an absolute path")]
    Relative {
        /// The path as the journal gives it.
        path: PathBuf,
    },
    /// A named file uses `..` below the sandbox's folder.
    #[error("msb's root-disk journal names {path:?}, which uses `..` inside the sandbox's folder")]
    ParentStep {
        /// The path as the journal gives it.
        path: PathBuf,
    },
    /// A named file lies outside the sandbox's folder (another sandbox's disk, any host file).
    #[error(
        "msb's root-disk journal names {path:?}, which is outside the sandbox's folder {sandbox:?}"
    )]
    Outside {
        /// The path as the journal gives it.
        path: PathBuf,
        /// The sandbox's folder, resolved.
        sandbox: PathBuf,
    },
    /// A named file is reached through a link inside the sandbox's folder.
    #[error("msb's root-disk journal names {path:?}, which goes through the link {link:?}")]
    Link {
        /// The path as the journal gives it.
        path: PathBuf,
        /// The link on the way.
        link: PathBuf,
    },
}

/// [`check`] as the error of a start: [`ComputeError::Runtime`] that says what the journal names
/// and why the start was refused.
///
/// # Errors
///
/// As [`check`].
pub(crate) fn check_before_start(sandbox_dir: &Path) -> Result<(), ComputeError> {
    check(sandbox_dir).map_err(|problem| {
        let why = if matches!(problem, JournalProblem::Unreadable { .. }) {
            "which files the start would attach cannot be told, so nothing was attached"
        } else {
            "a process in the sandbox can write that journal, and the start would attach what it \
             names as the writable root disk, so nothing was attached; removing the sandbox and \
             creating it again gives it a fresh root disk (its volumes are kept)"
        };
        ComputeError::Runtime {
            op: "start",
            message: format!("refused, {problem}: {why}"),
        }
    })
}

/// Checks the root-disk journal of the sandbox whose folder is `sandbox_dir`. No journal yet (the
/// first start, or a root disk msb does not journal) passes.
///
/// # Errors
///
/// [`JournalProblem`]: a file the journal names (a layer or the launch base) is not inside
/// `sandbox_dir` or is reached through a link, or the journal cannot be read.
pub(crate) fn check(sandbox_dir: &Path) -> Result<(), JournalProblem> {
    let journal = sandbox_dir.join("runtime").join(JOURNAL);
    let unreadable = |reason: String| JournalProblem::Unreadable {
        journal: journal.clone(),
        reason,
    };
    let meta = match fs::symlink_metadata(&journal) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    if meta.file_type().is_symlink() {
        return Err(JournalProblem::JournalIsLink { journal });
    }
    if meta.len() > MAX_JOURNAL_BYTES {
        return Err(unreadable(format!(
            "it holds {} bytes, more than msb accepts",
            meta.len()
        )));
    }
    let bytes = fs::read(&journal).map_err(|e| unreadable(e.to_string()))?;
    let parsed: Journal = serde_json::from_slice(&bytes).map_err(|e| unreadable(e.to_string()))?;
    let sandbox = fs::canonicalize(sandbox_dir).map_err(|e| unreadable(e.to_string()))?;
    let named = parsed
        .layers
        .into_iter()
        .map(|layer| layer.path)
        .chain(parsed.launch_base);
    for path in named {
        confined(path, &sandbox)?;
    }
    Ok(())
}

/// `path` lies in `sandbox` (a resolved folder) and no link inside `sandbox` is on its way.
///
/// The part above the sandbox's folder is compared resolved, not as written: msb writes the
/// folder the way it was given (on Windows maybe without the `\\?\` prefix or with short names,
/// anywhere maybe below a linked parent folder or with a `..`), and that is not the guest's to
/// choose. The anchor is the shortest leading part of the path that resolves into the
/// folder, and it must resolve to the folder itself: so no link or `..` the guest placed inside
/// the folder can be part of the anchor, where the walk below would not see it. Below the
/// anchor, no part may be `..` and every part must be a plain folder or file. A part that cannot
/// be read (missing, or below a file) ends the walk: msb runs as the same user and cannot open
/// anything below it either.
fn confined(path: PathBuf, sandbox: &Path) -> Result<(), JournalProblem> {
    if !path.is_absolute() {
        return Err(JournalProblem::Relative { path });
    }
    let mut leading: Vec<&Path> = path.ancestors().collect();
    leading.reverse();
    let Some(below) = leading
        .into_iter()
        .find_map(|a| {
            let resolved = fs::canonicalize(a).ok()?;
            resolved.starts_with(sandbox).then_some((a, resolved))
        })
        .filter(|(_, resolved)| resolved == sandbox)
        .and_then(|(anchor, _)| path.strip_prefix(anchor).ok())
    else {
        return Err(JournalProblem::Outside {
            path,
            sandbox: sandbox.to_path_buf(),
        });
    };
    if below.components().any(|c| c == Component::ParentDir) {
        return Err(JournalProblem::ParentStep { path });
    }
    let mut at = sandbox.to_path_buf();
    for part in below.components() {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(JournalProblem::Link { path, link: at });
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sandboxes folder with two sandboxes, `mine` (its `runtime/` folder made) and `other`
    /// (with a root disk).
    struct Home {
        _dir: tempfile::TempDir,
        sandboxes: PathBuf,
    }

    impl Home {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let sandboxes = dir.path().join("sandboxes");
            fs::create_dir_all(sandboxes.join("mine").join("runtime")).unwrap();
            fs::create_dir_all(sandboxes.join("other")).unwrap();
            fs::write(sandboxes.join("mine").join("upper.ext4"), b"mine").unwrap();
            fs::write(sandboxes.join("other").join("upper.ext4"), b"other").unwrap();
            Home {
                _dir: dir,
                sandboxes,
            }
        }

        fn mine(&self) -> PathBuf {
            self.sandboxes.join("mine")
        }

        /// Writes `mine`'s journal as msb does, with `layers` and an optional launch base.
        fn journal(&self, layers: &[&Path], launch_base: Option<&Path>) {
            let layers: Vec<_> = layers
                .iter()
                .enumerate()
                .map(|(i, path)| {
                    serde_json::json!({
                        "layer_id": format!("layer-{i:032x}"),
                        "path": path,
                        "format": if i == 0 { "raw" } else { "qcow2" },
                        "integrity_root": null,
                    })
                })
                .collect();
            let mut journal = serde_json::json!({
                "schema": "microsandbox.runtime-root-disk/1",
                "volume_id": "vol-00000000000000000000000000000000",
                "device_id": "vdb",
                "layout": "managed-upper",
                "published_generation": 0,
                "layers": layers,
            });
            if let Some(base) = launch_base {
                journal["launch_base"] = serde_json::json!(base);
            }
            self.write_journal(&serde_json::to_vec(&journal).unwrap());
        }

        fn write_journal(&self, bytes: &[u8]) {
            fs::write(self.mine().join("runtime").join(JOURNAL), bytes).unwrap();
        }
    }

    #[test]
    fn the_first_start_has_no_journal_and_passes() {
        let home = Home::new();
        assert_eq!(check(&home.mine()), Ok(()));
    }

    #[test]
    fn the_journal_msb_writes_passes_also_after_compaction_and_growth() {
        let home = Home::new();
        let upper = home.mine().join("upper.ext4");
        home.journal(&[&upper], None);
        assert_eq!(check(&home.mine()), Ok(()));

        // After a compaction or a growth the chain lives in staging folders of `runtime/`, and the
        // launch base stays the original upper, which may no longer exist.
        let compact = home.mine().join("runtime").join(".compact-abc");
        fs::create_dir_all(&compact).unwrap();
        fs::write(compact.join("upper-sealed-000.qcow2"), b"base").unwrap();
        let head = home.mine().join("upper-0123.qcow2");
        fs::write(&head, b"head").unwrap();
        fs::remove_file(&upper).unwrap();
        home.journal(
            &[&compact.join("upper-sealed-000.qcow2"), &head],
            Some(&upper),
        );
        assert_eq!(check(&home.mine()), Ok(()));
    }

    #[test]
    fn another_sandboxs_disk_is_refused() {
        let home = Home::new();
        let theirs = home.sandboxes.join("other").join("upper.ext4");
        let mine = home.mine().join("upper.ext4");
        home.journal(&[&theirs], Some(&mine));
        assert!(
            matches!(check(&home.mine()), Err(JournalProblem::Outside { path, .. }) if path == theirs)
        );
    }

    #[test]
    fn a_host_file_outside_every_sandbox_is_refused_also_as_the_launch_base() {
        let home = Home::new();
        let host_file = home.sandboxes.parent().unwrap().join("host-file");
        fs::write(&host_file, b"host").unwrap();
        let mine = home.mine().join("upper.ext4");
        home.journal(&[&host_file], Some(&mine));
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Outside { .. })
        ));

        home.journal(&[&mine], Some(&host_file));
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Outside { .. })
        ));
    }

    #[test]
    fn a_path_that_steps_out_with_dot_dot_is_refused() {
        let home = Home::new();
        let escape = home
            .mine()
            .join("runtime")
            .join("..")
            .join("..")
            .join("other")
            .join("upper.ext4");
        home.journal(&[&escape], None);
        assert_eq!(
            check(&home.mine()),
            Err(JournalProblem::ParentStep { path: escape })
        );

        // Below the folder msb never writes one, so one that lands back inside is refused too.
        fs::create_dir(home.mine().join("runtime").join(".compact-x")).unwrap();
        let back_in = home
            .mine()
            .join("runtime")
            .join(".compact-x")
            .join("..")
            .join("..")
            .join("upper.ext4");
        home.journal(&[&back_in], None);
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::ParentStep { .. })
        ));
    }

    #[test]
    fn a_home_spelled_with_dot_dot_passes() {
        // msb writes the folder the way it was given; a `..` above the sandbox is not the guest's.
        let home = Home::new();
        let spelled = home
            .sandboxes
            .join("other")
            .join("..")
            .join("mine")
            .join("upper.ext4");
        home.journal(&[&spelled], None);
        assert_eq!(check(&home.mine()), Ok(()));
    }

    #[test]
    fn a_relative_or_empty_path_is_refused() {
        let home = Home::new();
        home.journal(&[Path::new("upper.ext4")], None);
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Relative { .. })
        ));
        home.journal(&[Path::new("")], None);
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Relative { .. })
        ));
    }

    #[test]
    fn a_journal_that_is_not_msbs_shape_or_too_big_is_refused() {
        let home = Home::new();
        for bytes in [
            &b"not json"[..],
            br#"{"schema":"x"}"#,
            br#"{"layers":[{}]}"#,
        ] {
            home.write_journal(bytes);
            let got = check(&home.mine());
            assert!(
                matches!(got, Err(JournalProblem::Unreadable { .. })),
                "{got:?}"
            );
        }
        let big = vec![b' '; usize::try_from(MAX_JOURNAL_BYTES).unwrap() + 1];
        home.write_journal(&big);
        let problem = check(&home.mine()).unwrap_err();
        assert!(
            problem.to_string().contains("more than msb accepts"),
            "{problem}"
        );
    }

    #[test]
    fn a_journal_that_cannot_be_opened_is_refused() {
        let home = Home::new();
        // A folder in the journal's place cannot be read as a file.
        fs::create_dir(home.mine().join("runtime").join(JOURNAL)).unwrap();
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Unreadable { .. })
        ));
    }

    #[test]
    fn a_refused_start_says_what_the_journal_names_and_that_nothing_was_attached() {
        let home = Home::new();
        assert_eq!(check_before_start(&home.mine()), Ok(()));
        let theirs = home.sandboxes.join("other").join("upper.ext4");
        home.journal(&[&theirs], None);
        let refused = check_before_start(&home.mine()).unwrap_err();
        assert!(
            matches!(refused, ComputeError::Runtime { op: "start", .. }),
            "{refused:?}"
        );
        let message = refused.to_string();
        assert!(message.contains(&format!("{theirs:?}")), "{message}");
        assert!(
            message.contains("outside the sandbox's folder"),
            "{message}"
        );
        assert!(message.contains("nothing was attached"), "{message}");
        assert!(message.contains("creating it again"), "{message}");

        home.write_journal(b"not json");
        let message = check_before_start(&home.mine()).unwrap_err().to_string();
        assert!(message.contains("cannot be told"), "{message}");
        assert!(!message.contains("can write that journal"), "{message}");
    }

    #[test]
    fn problems_name_the_path_and_the_folder() {
        let problem = JournalProblem::Outside {
            path: PathBuf::from("/x/other/upper.ext4"),
            sandbox: PathBuf::from("/x/mine"),
        };
        let text = problem.to_string();
        assert!(text.contains("/x/other/upper.ext4"), "{text}");
        assert!(text.contains("/x/mine"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn a_journal_whose_folder_cannot_be_walked_is_refused() {
        // A file where `runtime/` belongs: looking up the journal fails with "not a directory",
        // which is not "no journal". (Windows reports that case as not found.)
        let home = Home::new();
        fs::remove_dir(home.mine().join("runtime")).unwrap();
        fs::write(home.mine().join("runtime"), b"").unwrap();
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Unreadable { .. })
        ));
    }

    #[cfg(unix)]
    mod links {
        use std::os::unix::fs::symlink;

        use super::*;

        #[test]
        fn a_layer_that_is_a_link_out_of_the_folder_is_refused() {
            let home = Home::new();
            let link = home.mine().join("runtime").join("disk.raw");
            symlink(home.sandboxes.join("other").join("upper.ext4"), &link).unwrap();
            home.journal(&[&link], None);
            assert_eq!(
                check(&home.mine()),
                Err(JournalProblem::Link {
                    path: link.clone(),
                    link: fs::canonicalize(home.mine())
                        .unwrap()
                        .join("runtime")
                        .join("disk.raw"),
                })
            );
        }

        #[test]
        fn a_linked_folder_on_the_way_is_refused() {
            let home = Home::new();
            symlink(
                home.sandboxes.join("other"),
                home.mine().join("runtime").join("d"),
            )
            .unwrap();
            let through = home.mine().join("runtime").join("d").join("upper.ext4");
            home.journal(&[&through], None);
            assert!(matches!(
                check(&home.mine()),
                Err(JournalProblem::Link { .. })
            ));

            // A link to `/` is outside too, however the rest of the path reads.
            symlink("/", home.mine().join("runtime").join("root")).unwrap();
            let host = home
                .mine()
                .join("runtime")
                .join("root")
                .join("etc")
                .join("passwd");
            home.journal(&[&host], None);
            assert!(matches!(
                check(&home.mine()),
                Err(JournalProblem::Link { .. })
            ));
        }

        #[test]
        fn a_dot_dot_after_a_link_in_the_folder_is_refused_even_when_it_lands_inside() {
            // Resolved now, `runtime/l/../../upper.ext4` is the sandbox's own disk; but the guest
            // can repoint `l`, so a path through it is never anchored past the link.
            let home = Home::new();
            fs::create_dir(home.mine().join("runtime").join("x")).unwrap();
            symlink(
                home.mine().join("runtime").join("x"),
                home.mine().join("runtime").join("l"),
            )
            .unwrap();
            let through = home
                .mine()
                .join("runtime")
                .join("l")
                .join("..")
                .join("..")
                .join("upper.ext4");
            home.journal(&[&through], None);
            assert!(matches!(
                check(&home.mine()),
                Err(JournalProblem::ParentStep { .. })
            ));
        }

        #[test]
        fn a_link_in_the_folder_back_to_the_folder_is_refused() {
            let home = Home::new();
            symlink(home.mine(), home.mine().join("runtime").join("me")).unwrap();
            let through = home.mine().join("runtime").join("me").join("upper.ext4");
            home.journal(&[&through], None);
            assert!(matches!(
                check(&home.mine()),
                Err(JournalProblem::Link { .. })
            ));
        }

        #[test]
        fn the_journal_as_a_link_is_refused() {
            let home = Home::new();
            let real = home.sandboxes.join("other").join("journal.json");
            fs::write(&real, br#"{"layers":[]}"#).unwrap();
            symlink(&real, home.mine().join("runtime").join(JOURNAL)).unwrap();
            assert!(matches!(
                check(&home.mine()),
                Err(JournalProblem::JournalIsLink { .. })
            ));
        }

        #[test]
        fn a_home_below_a_linked_folder_passes_written_either_way() {
            // The user's data folder may sit below a link (a moved home, a second disk). msb writes
            // the journal with the path it was given; the link is above the sandbox and fine.
            let home = Home::new();
            let alias = home.sandboxes.parent().unwrap().join("alias");
            symlink(&home.sandboxes, &alias).unwrap();
            let via_alias = alias.join("mine");
            home.journal(&[&via_alias.join("upper.ext4")], None);
            assert_eq!(check(&via_alias), Ok(()));
            assert_eq!(check(&home.mine()), Ok(()));
            // Through the alias, another sandbox is still another sandbox.
            home.journal(&[&alias.join("other").join("upper.ext4")], None);
            assert!(matches!(
                check(&via_alias),
                Err(JournalProblem::Outside { .. })
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_or_plain_spelling_of_the_folder_passes() {
        // `canonicalize` gives `\\?\C:\...`; msb writes the folder the way Puddle gave it.
        let home = Home::new();
        let verbatim = fs::canonicalize(home.mine()).unwrap().join("upper.ext4");
        assert!(
            verbatim.to_string_lossy().starts_with(r"\\?\"),
            "{verbatim:?}"
        );
        home.journal(&[&verbatim], None);
        assert_eq!(check(&home.mine()), Ok(()));
        home.journal(&[&home.mine().join("upper.ext4")], None);
        assert_eq!(check(&home.mine()), Ok(()));
        let theirs = fs::canonicalize(home.sandboxes.join("other"))
            .unwrap()
            .join("upper.ext4");
        home.journal(&[&theirs], None);
        assert!(matches!(
            check(&home.mine()),
            Err(JournalProblem::Outside { .. })
        ));
    }
}
