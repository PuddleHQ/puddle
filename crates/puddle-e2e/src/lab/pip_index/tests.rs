// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_checksum_is_the_zip_formats() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
}

#[test]
fn the_wheel_is_a_zip_of_three_stored_files_ending_in_a_directory() {
    let wheel = wheel("puddle-probe");
    assert!(wheel.starts_with(b"PK\x03\x04"));
    let end = &wheel[wheel.len() - 22..];
    assert_eq!(&end[..4], b"PK\x05\x06");
    assert_eq!(&end[10..12], 3u16.to_le_bytes(), "entries in the directory");
    let text = String::from_utf8_lossy(&wheel);
    for name in [
        "puddle_probe/__init__.py",
        "puddle_probe-1.0.0.dist-info/METADATA",
        "puddle_probe-1.0.0.dist-info/WHEEL",
    ] {
        assert_eq!(
            text.matches(name).count(),
            2,
            "{name} in header and directory"
        );
    }
}

fn ask(responder: &Responder, host: &str, target: &str) -> Option<WebAnswer> {
    responder(&crate::lab::Seen {
        method: "GET".into(),
        target: target.into(),
        headers: vec![("host".into(), host.into())],
        version: "HTTP/1.1",
        status: 200,
        body: Vec::new(),
    })
}

#[test]
fn the_index_lists_the_wheel_and_serves_it_on_its_own_host_only() {
    let index = pip_index("pypi.test", "puddle-probe");
    let name = wheel_name("puddle-probe");
    assert_eq!(name, "puddle_probe-1.0.0-py3-none-any.whl");

    let page = ask(&index, "pypi.test:443", "/simple/puddle-probe/?x=1").unwrap();
    assert_eq!((page.status, page.content_type), (200, "text/html"));
    assert!(
        String::from_utf8(page.body)
            .unwrap()
            .contains(&format!("/files/{name}"))
    );

    let file = ask(&index, "pypi.test", &format!("/files/{name}")).unwrap();
    assert_eq!(file.status, 200);
    assert_eq!(file.body, wheel("puddle-probe"));

    assert!(ask(&index, "pypi.test", "/other").is_none());
    assert!(ask(&index, "elsewhere.test", "/simple/puddle-probe/").is_none());
}
