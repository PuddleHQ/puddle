// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reads the stack a Windows program asks for out of its file, so a test can tell that the
//! `/STACK` link argument reached the binary without starting it.

/// The offset of the PE header's own offset in the DOS header.
const PE_OFFSET_FIELD: usize = 0x3c;
/// The COFF header sits after the 4-byte `PE\0\0` signature and is 20 bytes long.
const OPTIONAL_HEADER_OFFSET: usize = 24;
/// The magic of a 64-bit (PE32+) optional header.
const PE32_PLUS: u16 = 0x20b;
/// `SizeOfStackReserve` in a PE32+ optional header.
const STACK_RESERVE_OFFSET: usize = 72;

fn le_bytes<const N: usize>(image: &[u8], at: usize) -> Option<[u8; N]> {
    image.get(at..at.checked_add(N)?)?.try_into().ok()
}

/// The main-thread stack, in bytes, that a 64-bit Windows program file reserves (its
/// `SizeOfStackReserve`, which the linker's `/STACK` sets), or `None` when `image` is not one.
#[must_use]
pub fn pe_stack_reserve_bytes(image: &[u8]) -> Option<u64> {
    let pe = usize::try_from(u32::from_le_bytes(le_bytes(image, PE_OFFSET_FIELD)?)).ok()?;
    if image.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let optional = pe.checked_add(OPTIONAL_HEADER_OFFSET)?;
    if u16::from_le_bytes(le_bytes(image, optional)?) != PE32_PLUS {
        return None;
    }
    Some(u64::from_le_bytes(le_bytes(
        image,
        optional.checked_add(STACK_RESERVE_OFFSET)?,
    )?))
}

#[cfg(test)]
mod tests;
