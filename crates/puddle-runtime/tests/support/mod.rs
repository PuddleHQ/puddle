// SPDX-License-Identifier: GPL-3.0-or-later
//! Fixtures shared by the integration tests: a temp dir per test and fake msb binaries.

use std::path::Path;

use object::write::{Object, StandardSection};
use object::{Architecture, BinaryFormat, Endianness, SectionKind};

/// A directory removed on drop, named after `tag` so a leftover one says which test made it.
pub(crate) struct TempDir(tempfile::TempDir);

impl TempDir {
    pub(crate) fn new(tag: &str) -> Self {
        Self(
            tempfile::Builder::new()
                .prefix(&format!("puddle-runtime-{tag}-"))
                .tempdir()
                .unwrap(),
        )
    }

    pub(crate) fn path(&self) -> &Path {
        self.0.path()
    }
}

/// An object file shaped like msb's binary: a code section plus `.msbver` holding `version`
/// (none when `None`). Only its sections matter to the version reader.
pub(crate) fn fake_msb(version: Option<&str>) -> Vec<u8> {
    let format = if cfg!(windows) {
        BinaryFormat::Coff
    } else {
        BinaryFormat::Elf
    };
    let mut obj = Object::new(format, Architecture::X86_64, Endianness::Little);
    let text = obj.section_id(StandardSection::Text);
    obj.append_section_data(text, &[0xc3], 1);
    if let Some(v) = version {
        let id = obj.add_section(Vec::new(), b".msbver".to_vec(), SectionKind::ReadOnlyData);
        obj.append_section_data(id, v.as_bytes(), 1);
    }
    obj.write().unwrap()
}
