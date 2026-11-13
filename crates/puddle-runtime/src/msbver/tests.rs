// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use object::write::{Object as WriteObject, StandardSection};
use object::{Architecture, BinaryFormat, Endianness, SectionKind};

fn object_with(format: BinaryFormat, sections: &[&[u8]]) -> Vec<u8> {
    let mut obj = WriteObject::new(format, Architecture::X86_64, Endianness::Little);
    let text = obj.section_id(StandardSection::Text);
    obj.append_section_data(text, &[0xc3], 1);
    for data in sections {
        let id = obj.add_section(
            Vec::new(),
            VERSION_SECTION.into(),
            SectionKind::ReadOnlyData,
        );
        obj.append_section_data(id, data, 1);
    }
    obj.write().unwrap()
}

fn read(bytes: &[u8]) -> Result<Option<String>, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("msb");
    std::fs::write(&path, bytes).unwrap();
    read_embedded_version(&path)
}

#[test]
fn reads_the_version_from_elf_and_coff() {
    for format in [BinaryFormat::Elf, BinaryFormat::Coff] {
        let bytes = object_with(format, &[b"0.7.7-puddle.1"]);
        assert_eq!(
            read(&bytes),
            Ok(Some("0.7.7-puddle.1".into())),
            "{format:?}"
        );
    }
}

#[test]
fn no_section_is_none() {
    assert_eq!(read(&object_with(BinaryFormat::Elf, &[])), Ok(None));
    assert_eq!(read(&object_with(BinaryFormat::Coff, &[])), Ok(None));
}

#[test]
fn malformed_sections_are_errors() {
    let two = object_with(BinaryFormat::Elf, &[b"0.7.7", b"0.7.8"]);
    assert!(read(&two).unwrap_err().contains("more than one"));
    let big = vec![b'1'; 257];
    let err = read(&object_with(BinaryFormat::Elf, &[&big])).unwrap_err();
    assert!(err.contains("1 to 256 bytes"), "{err}");
    let not_utf8 = object_with(BinaryFormat::Elf, &[&[0xff, 0xfe]]);
    assert!(read(&not_utf8).unwrap_err().contains("UTF-8"));
    let nul = object_with(BinaryFormat::Elf, &[&[0, 0]]);
    assert!(read(&nul).unwrap_err().contains("empty"));
}

#[test]
fn exactly_256_bytes_is_fine() {
    let max = vec![b'7'; 256];
    let got = read(&object_with(BinaryFormat::Elf, &[&max])).unwrap();
    assert_eq!(got.map(|s| s.len()), Some(256));
}

#[test]
fn non_executables_and_missing_files_are_errors() {
    assert!(read(b"#!/bin/sh\necho 0.7.7-puddle.1\n").is_err());
    assert!(read(b"").is_err());
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    let missing = read_embedded_version(Path::new("/nonexistent/puddle/msb"));
    assert!(missing.is_err());
}
