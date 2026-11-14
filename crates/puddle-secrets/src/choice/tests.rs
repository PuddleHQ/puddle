// SPDX-License-Identifier: AGPL-3.0-or-later
#![allow(clippy::unwrap_used, reason = "test code: a panic is how a test fails")]
use super::*;

fn inputs(option: StoreOption, recorded: Option<StoreKind>) -> Inputs {
    Inputs {
        option,
        recorded,
        names_secret: false,
        linux: true,
        headless: false,
    }
}

fn pick(inputs: Inputs) -> (StoreKind, Rule, Recording) {
    let d = decide(inputs).unwrap();
    (d.store, d.rule, d.recording)
}

#[test]
fn the_first_start_follows_the_rules_in_order() {
    use Recording::{Now, WhenWorking};
    use StoreKind::{File as InFile, Os as InOs};
    use StoreOption::{Auto, File, Os};
    // (option, names a secret, linux, headless) -> store, rule, recording
    let rows = [
        // The option comes first, whatever else is true.
        (File, true, false, false, (InFile, Rule::Option, Now)),
        (Os, false, true, true, (InOs, Rule::Option, Now)),
        // Rows that name a secret mean the values are in the OS store, even headless.
        (Auto, true, true, true, (InOs, Rule::ExistingSecrets, Now)),
        // Windows and macOS.
        (Auto, false, false, false, (InOs, Rule::OtherPlatform, Now)),
        (Auto, false, false, true, (InOs, Rule::OtherPlatform, Now)),
        // Linux: the flag decides.
        (Auto, false, true, true, (InFile, Rule::Headless, Now)),
        (
            Auto,
            false,
            true,
            false,
            (InOs, Rule::LinuxDefault, WhenWorking),
        ),
    ];
    for (option, names_secret, linux, headless, want) in rows {
        let got = pick(Inputs {
            option,
            recorded: None,
            names_secret,
            linux,
            headless,
        });
        assert_eq!(
            got, want,
            "option {option:?}, names a secret {names_secret}, linux {linux}, headless {headless}"
        );
    }
}

#[test]
fn a_recorded_store_is_never_switched() {
    for recorded in [StoreKind::Os, StoreKind::File] {
        for headless in [false, true] {
            for names_secret in [false, true] {
                let got = pick(Inputs {
                    names_secret,
                    headless,
                    ..inputs(StoreOption::Auto, Some(recorded))
                });
                assert_eq!(got, (recorded, Rule::Recorded, Recording::Already));
            }
        }
    }
    // The option naming the recorded store is the same as no option.
    assert_eq!(
        pick(inputs(StoreOption::File, Some(StoreKind::File))).0,
        StoreKind::File
    );
}

#[test]
fn an_option_that_names_the_other_store_stops_the_start_and_says_what_to_do() {
    let err = decide(inputs(StoreOption::File, Some(StoreKind::Os))).unwrap_err();
    assert_eq!(
        err,
        ChoiceError::Mismatch {
            recorded: StoreKind::Os,
            asked: StoreKind::File
        }
    );
    let text = err.to_string();
    assert!(
        text.contains("the operating system's credential store"),
        "{text}"
    );
    assert!(text.contains("Puddle's file vault"), "{text}");
    assert!(text.contains("`auto` or `os`"), "{text}");
    let back = decide(inputs(StoreOption::Os, Some(StoreKind::File))).unwrap_err();
    assert!(back.to_string().contains("`auto` or `file`"));
}

#[test]
fn the_option_reads_its_three_values_and_nothing_else() {
    assert_eq!(StoreOption::parse("auto"), Some(StoreOption::Auto));
    assert_eq!(StoreOption::parse("os"), Some(StoreOption::Os));
    assert_eq!(StoreOption::parse("file"), Some(StoreOption::File));
    for bad in ["", "AUTO", "keyring", " os", "file "] {
        assert_eq!(StoreOption::parse(bad), None, "{bad:?}");
    }
}

#[test]
fn a_record_written_is_the_record_read_back() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(read_record(dir.path()), Ok(None));
    write_record(dir.path(), StoreKind::File, Rule::Headless).unwrap();
    let record = read_record(dir.path()).unwrap().unwrap();
    assert_eq!(record.version, 1);
    assert_eq!(record.store, StoreKind::File);
    assert_eq!(record.why, Rule::Headless.why());
    assert!(
        record.decided.len() == 20 && record.decided.ends_with('Z'),
        "{}",
        record.decided
    );
    let text = std::fs::read_to_string(secrets_folder(dir.path()).join("store.json")).unwrap();
    assert!(text.contains("\"store\": \"file\""), "{text}");
}

#[cfg(unix)]
#[test]
fn the_record_sits_in_an_owner_only_folder() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    write_record(dir.path(), StoreKind::Os, Rule::Option).unwrap();
    let mode = std::fs::metadata(secrets_folder(dir.path()))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");
}

#[test]
fn a_record_it_cannot_read_is_an_error_never_a_new_choice() {
    let dir = tempfile::tempdir().unwrap();
    let folder = secrets_folder(dir.path());
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("store.json");
    let cases = [
        "",
        "not json",
        r#"{"version":1,"store":"cloud","decided":"x","why":"y"}"#,
        r#"{"version":2,"store":"os","decided":"x","why":"y"}"#,
        r#"{"version":1,"store":"os","decided":"x","why":"y","extra":1}"#,
        r#"{"store":"os"}"#,
    ];
    for body in cases {
        std::fs::write(&path, body).unwrap();
        assert_eq!(
            read_record(dir.path()),
            Err(RecordError::Unreadable),
            "{body:?}"
        );
    }
    // A folder where the file should be is an I/O error, not "no record".
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(read_record(dir.path()), Err(RecordError::Io));
}

#[test]
fn a_time_is_written_in_utc_to_the_second() {
    let at = |secs| UNIX_EPOCH + std::time::Duration::from_secs(secs);
    assert_eq!(utc(at(0)), "1970-01-01T00:00:00Z");
    assert_eq!(utc(at(951_782_400)), "2000-02-29T00:00:00Z");
    assert_eq!(utc(at(1_794_634_435)), "2026-11-14T05:33:55Z");
    assert_eq!(utc(at(4_102_444_799)), "2099-12-31T23:59:59Z");
    // The century rules of the calendar: 2100 and 2200 are not leap years, 2400 is.
    assert_eq!(utc(at(4_107_542_400)), "2100-03-01T00:00:00Z");
    assert_eq!(utc(at(4_107_628_800)), "2100-03-02T00:00:00Z");
    assert_eq!(utc(at(7_263_216_000)), "2200-03-01T00:00:00Z");
    assert_eq!(utc(at(13_574_563_200)), "2400-02-29T00:00:00Z");
    assert_eq!(utc(at(13_574_649_600)), "2400-03-01T00:00:00Z");
}

#[test]
fn the_vault_and_the_record_share_one_folder() {
    let data = Path::new("/srv/puddle");
    assert_eq!(vault_path(data), Path::new("/srv/puddle/secrets/vault"));
    assert_eq!(secrets_folder(data), Path::new("/srv/puddle/secrets"));
}

#[test]
fn every_rule_and_store_has_the_words_a_record_and_a_message_print() {
    for (option, text) in [
        (StoreOption::Auto, "auto"),
        (StoreOption::Os, "os"),
        (StoreOption::File, "file"),
    ] {
        assert_eq!(option.as_str(), text);
        assert_eq!(StoreOption::parse(text), Some(option));
    }
    let rules = [
        Rule::Recorded,
        Rule::Option,
        Rule::ExistingSecrets,
        Rule::OtherPlatform,
        Rule::Headless,
        Rule::LinuxDefault,
    ];
    let whys: std::collections::HashSet<_> = rules.iter().map(|r| r.why()).collect();
    assert_eq!(
        whys.len(),
        rules.len(),
        "each rule says something of its own"
    );
    assert_eq!(StoreKind::Os.to_string(), StoreKind::Os.describe());
    assert_eq!(StoreKind::File.to_string(), "Puddle's file vault");
}
