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
