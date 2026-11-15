// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use super::*;

fn report(message: &str) -> Report {
    Report {
        version: "1.2.3".to_owned(),
        platform: "linux x86_64".to_owned(),
        thread: "main".to_owned(),
        location: "src/shell.rs:10:5".to_owned(),
        message: message.to_owned(),
        backtrace: "0: puddle_app::run".to_owned(),
    }
}

#[test]
fn the_file_says_what_crashed_and_that_nothing_was_sent() {
    let text = report("boom").render();
    assert!(text.contains("nothing was sent anywhere"), "{text}");
    for line in [
        "version:  1.2.3",
        "platform: linux x86_64",
        "thread:   main",
        "at:       src/shell.rs:10:5",
        "message:  boom",
        "0: puddle_app::run",
    ] {
        assert!(text.contains(line), "{line} missing from {text}");
    }
}

#[test]
fn a_report_is_written_into_a_folder_that_did_not_exist() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join(FOLDER);
    let path = write(
        &folder,
        &report("boom"),
        UNIX_EPOCH + Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(path.parent().unwrap(), folder);
    let name = path.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.starts_with("crash-00000000000005000-"), "{name}");
    assert!(fs::read_to_string(path).unwrap().contains("message:  boom"));
}

#[test]
fn only_the_newest_files_are_kept_and_other_files_are_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("notes.txt"), "mine").unwrap();
    for second in 0..(KEEP as u64 + 3) {
        write(
            dir.path(),
            &report(&format!("crash {second}")),
            UNIX_EPOCH + Duration::from_secs(second),
        )
        .unwrap();
    }
    let mut names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names.len(), KEEP + 1, "{names:?}");
    assert!(names.contains(&"notes.txt".to_owned()));
    let oldest_kept = names.iter().find(|n| n.starts_with("crash-")).unwrap();
    assert!(
        oldest_kept.starts_with("crash-00000000000003000-"),
        "{oldest_kept}"
    );
}

#[test]
fn a_second_report_in_the_same_millisecond_of_one_process_is_refused_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let at = UNIX_EPOCH + Duration::from_secs(1);
    write(dir.path(), &report("first"), at).unwrap();
    let err = write(dir.path(), &report("second"), at).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn pruning_a_folder_that_is_gone_does_nothing() {
    prune(&tempfile::tempdir().unwrap().path().join("gone"), 1);
}
