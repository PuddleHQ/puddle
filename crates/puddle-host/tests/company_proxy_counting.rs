// SPDX-License-Identifier: AGPL-3.0-or-later
//! The VM tests that send the host's upstream traffic through a stand-in company proxy (the
//! `through_company_proxy*` helpers) share that proxy with the host's own background downloads
//! (mise's setup after the first start leaves for `mise.jdx.dev:443` through the same tunnel while
//! a test runs). An assertion that counts every `CONNECT` the proxy saw (`carried()`,
//! `connects()`) therefore passes or fails by timing. A VM test counts by the name under test
//! (`PrivateAccess::carried_to`), or filters the list itself and says so with `// by-name`.
//! This guard fails when a VM test calls the unfiltered accessors.

use std::path::Path;

/// The lines of `source` that read the proxy's whole `CONNECT` list without naming a host.
fn unfiltered_counts(source: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter(|(i, line)| {
            let code = line.trim_start();
            !code.starts_with("//")
                && (code.contains(".carried()") || code.contains(".connects()"))
                // A discard (`let _ = proxy.connects();`) counts nothing.
                && !code.starts_with("let _ =")
                && !line.contains("// by-name")
                && !(*i > 0 && lines[i - 1].trim() == "// by-name")
        })
        .map(|(i, line)| (i + 1, line.trim().to_owned()))
        .collect()
}

#[test]
fn no_vm_test_counts_what_the_company_proxy_carried_without_naming_a_host() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut offenders = Vec::new();
    let mut files = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !(name.starts_with("vm_") && name.ends_with(".rs")) {
            continue;
        }
        files += 1;
        let source = std::fs::read_to_string(&path).unwrap();
        for (line, text) in unfiltered_counts(&source) {
            offenders.push(format!("{name}:{line}: {text}"));
        }
    }
    assert!(files > 10, "the guard found only {files} VM test files");
    assert!(
        offenders.is_empty(),
        "these assertions count every host the shared proxy carried, including the host's own \
         background downloads; use `PrivateAccess::carried_to(name)`:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_scan_flags_an_unfiltered_count_and_passes_the_named_and_discarded_ones() {
    let source = "\
assert!(access.carried().is_empty());
let n = proxy.connects().len();
let _ = net.proxy.connects();
assert!(access.carried_to(NAME).is_empty());
// access.carried() in a comment
assert!(access.carried().is_empty()); // by-name
// by-name
let all = proxy.connects();
";
    assert_eq!(
        unfiltered_counts(source),
        [
            (1, "assert!(access.carried().is_empty());".to_owned()),
            (2, "let n = proxy.connects().len();".to_owned()),
        ]
    );
}
