// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reads the version msb embeds in its own binary, without running it.
//!
//! msb's CLI (since 0.7.6) stores its Cargo package version as plain UTF-8 in a section named
//! `.msbver` (ELF and PE). The SDK's `setup::resolve_runtime_version` reads the same section, but
//! the SDK only exposes it with its `local` feature, i.e. the whole runtime client; this crate
//! stays independent of the SDK so it builds and tests without it. The two readers
//! agree on the format: the section's bytes are the version, 1 to [`MAX_VERSION_BYTES`] long.

use std::fs::File;
use std::path::Path;

use object::read::ReadCache;
use object::{Object, ObjectSection};

/// The section msb's binary carries its version in.
pub const VERSION_SECTION: &str = ".msbver";

/// The largest version section accepted (msb's own limit).
pub const MAX_VERSION_BYTES: u64 = 256;

/// Reads the embedded runtime version of the executable at `path`.
///
/// Only the headers, the section table and the version section are read (through a cache), never
/// the whole 60 MB binary, and nothing is executed. `Ok(None)` means the file is a valid ELF, PE
/// or COFF file without a version section.
///
/// # Errors
///
/// A message when the file can't be opened, isn't an ELF/PE/COFF file, or has more than one,
/// an empty, an oversized or a non-UTF-8 version section.
pub fn read_embedded_version(path: &Path) -> Result<Option<String>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let cache = ReadCache::new(file);
    let parsed = object::File::parse(&cache).map_err(|e| e.to_string())?;
    let mut sections = parsed.sections().filter(|s| {
        s.name_bytes()
            .is_ok_and(|n| n == VERSION_SECTION.as_bytes())
    });
    let Some(section) = sections.next() else {
        return Ok(None);
    };
    if sections.next().is_some() {
        return Err("more than one version section".into());
    }
    let size = section.size();
    if size == 0 || size > MAX_VERSION_BYTES {
        return Err(format!(
            "version section must hold 1 to {MAX_VERSION_BYTES} bytes, has {size}"
        ));
    }
    let bytes = section.data().map_err(|e| e.to_string())?;
    // PE pads raw section data to the file alignment; the version is the bytes before padding.
    let used = bytes
        .get(..usize::try_from(size).unwrap_or(usize::MAX))
        .unwrap_or(bytes);
    let text = std::str::from_utf8(used).map_err(|_| "version section is not UTF-8".to_owned())?;
    let text = text.trim_end_matches('\0');
    if text.is_empty() {
        return Err("version section is empty".into());
    }
    Ok(Some(text.to_owned()))
}

#[cfg(test)]
mod tests;
