// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake Python package index for [`WebHost`](super::WebHost): the "simple" page of one project
//! and the one wheel it links to, so a test can have `pip download` fetch a package without a
//! public index being up. Test code only.

use std::sync::Arc;

use super::{Responder, WebAnswer};

/// The version the fake wheel carries.
const VERSION: &str = "1.0.0";

/// The wheel's file name.
fn wheel_name(project: &str) -> String {
    format!("{}-{VERSION}-py3-none-any.whl", project.replace('-', "_"))
}

/// The CRC-32 of `data` (the zip format's checksum).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

/// A zip archive holding `files` stored uncompressed (a wheel is one).
fn zip_stored(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in files {
        let offset = u32::try_from(out.len()).unwrap();
        let (crc, size, name_len) = (
            crc32(data),
            u32::try_from(data.len()).unwrap(),
            u16::try_from(name.len()).unwrap(),
        );
        let header = |out: &mut Vec<u8>, signature: u32, central: bool| {
            out.extend(signature.to_le_bytes());
            if central {
                out.extend(20u16.to_le_bytes()); // made by
            }
            out.extend(20u16.to_le_bytes()); // needed to extract
            out.extend(0u16.to_le_bytes()); // flags
            out.extend(0u16.to_le_bytes()); // stored
            out.extend(0u16.to_le_bytes()); // time
            out.extend(0x21u16.to_le_bytes()); // date: 1980-01-01
            out.extend(crc.to_le_bytes());
            out.extend(size.to_le_bytes()); // compressed
            out.extend(size.to_le_bytes()); // uncompressed
            out.extend(name_len.to_le_bytes());
            out.extend(0u16.to_le_bytes()); // extra
        };
        header(&mut out, 0x0403_4b50, false);
        out.extend(name.as_bytes());
        out.extend(data);
        header(&mut directory, 0x0201_4b50, true);
        directory.extend(0u16.to_le_bytes()); // comment
        directory.extend(0u16.to_le_bytes()); // disk
        directory.extend(0u16.to_le_bytes()); // internal attributes
        directory.extend(0u32.to_le_bytes()); // external attributes
        directory.extend(offset.to_le_bytes());
        directory.extend(name.as_bytes());
    }
    let (start, size) = (
        u32::try_from(out.len()).unwrap(),
        u32::try_from(directory.len()).unwrap(),
    );
    let count = u16::try_from(files.len()).unwrap();
    out.extend(directory);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]); // disk numbers
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(size.to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes()); // comment
    out
}

/// The wheel of `project`: a package of that name with one module, and the metadata `pip` reads.
fn wheel(project: &str) -> Vec<u8> {
    let module = project.replace('-', "_");
    let dist_info = format!("{module}-{VERSION}.dist-info");
    zip_stored(&[
        (
            format!("{module}/__init__.py"),
            b"MARKER = 'fake index'\n".to_vec(),
        ),
        (
            format!("{dist_info}/METADATA"),
            format!("Metadata-Version: 2.1\nName: {project}\nVersion: {VERSION}\n").into_bytes(),
        ),
        (
            format!("{dist_info}/WHEEL"),
            b"Wheel-Version: 1.0\nGenerator: lab\nRoot-Is-Purelib: true\nTag: py3-none-any\n"
                .to_vec(),
        ),
    ])
}

/// The answers of a package index for one `project`: `/simple/<project>/` lists the wheel and
/// `/files/<wheel>` serves it. Every other request gets the website's default answer, and a site
/// that is not `host` is left alone.
#[must_use]
pub fn pip_index(host: &'static str, project: &'static str) -> Responder {
    let wheel_name = wheel_name(project);
    let wheel = wheel(project);
    Arc::new(move |seen| {
        let on_host = seen
            .headers_named("host")
            .first()
            .is_some_and(|h| h.split(':').next() == Some(host));
        if !on_host {
            return None;
        }
        let path = seen.target.split('?').next().unwrap_or_default();
        if path == format!("/simple/{project}/") {
            Some(WebAnswer {
                status: 200,
                content_type: "text/html",
                body: format!(
                    "<!DOCTYPE html><html><body><a href=\"/files/{wheel_name}\">{wheel_name}</a></body></html>\n"
                )
                .into_bytes(),
            })
        } else if path == format!("/files/{wheel_name}") {
            Some(WebAnswer {
                status: 200,
                content_type: "application/octet-stream",
                body: wheel.clone(),
            })
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
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
}
