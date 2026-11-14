// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn set(tools: &[&str]) -> BTreeSet<String> {
    tools.iter().map(|t| (*t).to_owned()).collect()
}

#[test]
fn a_report_holds_its_three_kinds_of_record_and_ends_with_end() {
    let report =
        parse_report("missing node@22.11.0\nmissing go@1.23.0\ninstalled node@22.11.0\nfailed go@1.23.0\nend\n\n")
            .unwrap();
    assert_eq!(report.missing, set(&["go@1.23.0", "node@22.11.0"]));
    assert_eq!(report.installed, set(&["node@22.11.0"]));
    assert_eq!(report.failed, set(&["go@1.23.0"]));
    assert_eq!(parse_report("end\n"), Some(Report::default()));
}

#[test]
fn a_report_that_is_not_exactly_the_agents_is_refused_whole() {
    let long = format!("missing {}\nend\n", "x".repeat(MAX_TOOL_CHARS + 1));
    let many = format!("{}end\n", "missing a@1\n".repeat(MAX_RECORDS + 2));
    for bad in [
        "",
        "missing node@22\n",
        "missing node@22\nend\nmissing go@1\n",
        "removed node@22\nend\n",
        "missing \nend\n",
        "missing node 22\nend\n",
        "missing node\u{1b}[2J\nend\n",
        "missing\nend\n",
        long.as_str(),
        many.as_str(),
    ] {
        assert_eq!(parse_report(bad), None, "{bad:?}");
    }
    let at_the_cap = format!("{}end\n", "missing a@1\n".repeat(MAX_RECORDS));
    assert!(parse_report(&at_the_cap).is_some());
}

#[test]
fn a_good_install_has_no_reason_and_a_failed_one_quotes_mises_last_line() {
    let good = ExecOutput::new(
        0,
        "missing java@temurin-21\ninstalled java@temurin-21\nend\n",
        "",
    );
    let (report, reason) = read_install(Ok(good));
    assert_eq!(reason, None);
    assert!(has_java(&report.installed));
    let failed = ExecOutput::new(
        1,
        "missing go@1.23.0\nfailed go@1.23.0\nend\n",
        "mise WARN slow\nmise ERROR HTTP 403 for https://dl.google.com/go\n\n",
    );
    let (report, reason) = read_install(Ok(failed));
    assert_eq!(report.failed, set(&["go@1.23.0"]));
    assert_eq!(
        reason.as_deref(),
        Some("mise ERROR HTTP 403 for https://dl.google.com/go")
    );
    assert!(!has_java(&report.failed));
}

#[test]
fn an_install_without_words_says_its_exit_code_and_an_unreadable_one_says_so() {
    let silent = ExecOutput::new(137, "failed go@1\nend\n", "");
    assert_eq!(
        read_install(Ok(silent)).1.as_deref(),
        Some("the install ended with exit code 137")
    );
    let missing_but_zero = ExecOutput::new(0, "failed go@1\nend\n", "");
    assert!(read_install(Ok(missing_but_zero)).1.is_some());
    let garbled = ExecOutput::new(0, "hello\n", "");
    let (report, reason) = read_install(Ok(garbled));
    assert_eq!(report, Report::default());
    assert_eq!(
        reason.as_deref(),
        Some("the guest's report could not be read (exit code 0)")
    );
    let err = ComputeError::NotFound {
        sandbox: "acme".to_owned(),
    };
    assert_eq!(
        read_install(Err(err)).1.as_deref(),
        Some("sandbox \"acme\" not found")
    );
}

#[test]
fn counts_beyond_u32_are_capped() {
    assert_eq!(count(3), 3);
    assert_eq!(count(usize::MAX), u32::MAX);
}
