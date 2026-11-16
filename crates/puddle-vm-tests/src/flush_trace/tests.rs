// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt::Write as _;

use super::*;

fn event(id: u32, pid: u32, fields: &[(&str, &str)]) -> String {
    let mut data = String::new();
    for (n, v) in fields {
        write!(data, "<Data Name=\"{n}\">{v}</Data>").unwrap();
    }
    format!(
        "<Event xmlns=\"x\"><System><EventID>{id}</EventID><Execution ProcessID=\"{pid}\" ThreadID=\"9\"/></System><EventData>{data}</EventData></Event>\n"
    )
}

fn create(object: &str, file: &str) -> String {
    event(CREATE, 100, &[("FileObject", object), ("FileName", file)])
}

fn flush(object: &str, pid: u32) -> String {
    event(FLUSH, pid, &[("FileObject", object), ("FileKey", "0x0")])
}

const IMAGE: &str = r"\Device\HarddiskVolume3\pvm\vols\w1\disk.raw";

#[test]
fn counts_only_flushes_of_the_image_between_the_markers() {
    let xml = [
        create("0xA", IMAGE),
        flush("0xA", 7),
        create("0xB", r"\Device\X\mark-start"),
        flush("0xA", 7),
        flush("0xA", 7),
        flush("0xC", 7),
        create("0xC", r"\Device\X\other.txt"),
        flush("0xC", 7),
        flush("0xA", 8),
        create("0xD", r"\Device\X\mark-end"),
        flush("0xA", 7),
    ]
    .concat();
    let got = flushes_between(&xml, "disk.raw", "mark-start", "mark-end").unwrap();
    assert_eq!(got.total, 3);
    assert_eq!(got.by_process, BTreeMap::from([(7, 2), (8, 1)]));
    assert_eq!(got.event_ids.get(&FLUSH), Some(&7));
}

#[test]
fn single_quoted_attributes_and_names_parse() {
    let xml = "<Event><System><EventID Qualifiers='0'>12</EventID><Execution ProcessID='5'/></System><EventData><Data Name='FileObject'>0x1</Data><Data Name='FileName'>disk.raw</Data></EventData></Event>";
    assert_eq!(event_id(xml), Some(12));
    assert_eq!(process_id(xml), Some(5));
    assert_eq!(data(xml, "FileName"), Some("disk.raw"));
}

#[test]
fn an_empty_window_is_a_zero_not_an_error() {
    let xml = [
        create("0xA", IMAGE),
        create("0xB", "mark-start"),
        create("0xD", "mark-end"),
    ]
    .concat();
    let got = flushes_between(&xml, "disk.raw", "mark-start", "mark-end").unwrap();
    assert_eq!(got.total, 0);
}

#[test]
fn a_trace_that_never_saw_the_image_or_a_marker_is_an_error() {
    let no_image = [create("0xB", "mark-start"), create("0xD", "mark-end")].concat();
    let e = flushes_between(&no_image, "disk.raw", "mark-start", "mark-end").unwrap_err();
    assert!(e.to_string().contains("the image"), "{e}");
    let no_end = [create("0xA", IMAGE), create("0xB", "mark-start")].concat();
    let e = flushes_between(&no_end, "disk.raw", "mark-start", "mark-end").unwrap_err();
    assert!(e.to_string().contains("the end marker"), "{e}");
}

#[cfg(unix)]
mod session {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// A shell script that stands in for a Windows tool.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// `logman` appends its arguments to a log; `tracerpt` writes `xml` to the `-o` path.
    fn tools(dir: &Path, xml: &str) -> Tools {
        Tools {
            logman: script(
                dir,
                "logman",
                &format!("echo \"$@\" >> {}/calls", dir.display()),
            ),
            tracerpt: script(dir, "tracerpt", &format!("printf '%s' '{xml}' > \"$3\"")),
        }
    }

    #[test]
    fn a_session_starts_on_the_file_provider_and_stop_returns_the_xml() {
        let dir = tempfile::tempdir().unwrap();
        let trace =
            FlushTrace::start_with(tools(dir.path(), "<Event>x"), dir.path(), "s1").unwrap();
        let xml = trace.stop().unwrap();
        assert_eq!(xml, "<Event>x");
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        let lines: Vec<_> = calls.lines().collect();
        assert_eq!(lines.len(), 2, "{calls}");
        assert!(lines[0].starts_with("start s1 -p Microsoft-Windows-Kernel-File 0xF0"));
        assert!(lines[0].ends_with("-ets"));
        assert_eq!(lines[1], "stop s1 -ets");
    }

    #[test]
    fn a_dropped_session_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        drop(FlushTrace::start_with(tools(dir.path(), ""), dir.path(), "s2").unwrap());
        let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
        assert!(calls.ends_with("stop s2 -ets\n"), "{calls}");
    }

    #[test]
    fn a_failing_tool_is_a_trace_error_with_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let broken = Tools {
            logman: script(dir.path(), "logman", "echo denied >&2; exit 5"),
            tracerpt: PathBuf::from("tracerpt"),
        };
        let e = FlushTrace::start_with(broken, dir.path(), "s3").unwrap_err();
        assert!(matches!(e, HarnessError::Trace { .. }), "{e}");
        assert!(e.to_string().contains("denied"), "{e}");
    }

    #[test]
    fn a_missing_tool_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Tools {
            logman: dir.path().join("no-such-logman"),
            tracerpt: PathBuf::from("tracerpt"),
        };
        let e = FlushTrace::start_with(missing, dir.path(), "s4").unwrap_err();
        assert!(matches!(e, HarnessError::Io { .. }), "{e}");
    }

    #[test]
    fn stop_without_a_trace_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools {
            logman: script(dir.path(), "logman", "true"),
            tracerpt: script(dir.path(), "tracerpt", "true"),
        };
        let e = FlushTrace::start_with(tools, dir.path(), "s5")
            .unwrap()
            .stop()
            .unwrap_err();
        assert!(matches!(e, HarnessError::Io { .. }), "{e}");
    }

    #[test]
    fn a_failing_tracerpt_is_a_trace_error() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools {
            logman: script(dir.path(), "logman", "true"),
            tracerpt: script(dir.path(), "tracerpt", "echo no events >&2; exit 1"),
        };
        let e = FlushTrace::start_with(tools, dir.path(), "s6")
            .unwrap()
            .stop()
            .unwrap_err();
        assert!(matches!(e, HarnessError::Trace { .. }), "{e}");
        assert!(e.to_string().contains("no events"), "{e}");
    }

    /// The real tool names: on a machine without `logman` the session does not start.
    #[test]
    fn the_default_tools_are_the_windows_ones() {
        let dir = tempfile::tempdir().unwrap();
        let e = FlushTrace::start(dir.path(), "s7").unwrap_err();
        assert!(matches!(e, HarnessError::Io { .. }), "{e}");
    }
}
