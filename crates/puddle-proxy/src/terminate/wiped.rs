// SPDX-License-Identifier: AGPL-3.0-or-later
//! A byte buffer for what may hold a secret: it grows by copying into a new buffer and
//! overwriting the old one, and it is overwritten when it drops, also after it became [`Bytes`].

use std::io;

use bytes::Bytes;
use zeroize::{Zeroize, Zeroizing};

/// The first size of a buffer; a token answer fits in it.
const FIRST: usize = 2048;

/// Bytes overwritten when dropped. A plain `Vec` or `BytesMut` that grows frees each smaller copy
/// as it is, and `Bytes::copy_from_slice` leaves the source behind: this does neither.
pub(crate) struct Wiped(Vec<u8>);

impl Wiped {
    /// An empty buffer with room for `capacity` bytes.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity.max(1)))
    }

    /// An empty buffer of the usual first size.
    pub(crate) fn new() -> Self {
        Self::with_capacity(FIRST)
    }

    /// Copies `bytes` into a buffer of exactly their size (the caller keeps, and wipes, its own).
    pub(crate) fn copy_of(bytes: &[u8]) -> Self {
        let mut wiped = Self::with_capacity(bytes.len());
        wiped.0.extend_from_slice(bytes);
        wiped
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// Appends `bytes`. When they do not fit, the next buffer is made first and the old one is
    /// overwritten after the copy.
    pub(crate) fn extend_from_slice(&mut self, bytes: &[u8]) {
        let needed = self.0.len() + bytes.len();
        if needed > self.0.capacity() {
            let mut bigger = Vec::with_capacity(needed.max(self.0.capacity() * 2));
            bigger.extend_from_slice(&self.0);
            self.0.zeroize();
            self.0 = bigger;
        }
        self.0.extend_from_slice(bytes);
    }

    /// The bytes as a shared buffer; the last clone to drop overwrites them.
    pub(crate) fn into_bytes(self) -> Bytes {
        Bytes::from_owner(self)
    }

    /// The bytes as a vector that overwrites itself when dropped.
    pub(crate) fn into_zeroizing(mut self) -> Zeroizing<Vec<u8>> {
        Zeroizing::new(std::mem::take(&mut self.0))
    }
}

impl AsRef<[u8]> for Wiped {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl io::Write for Wiped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Wiped {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[cfg(test)]
mod tests;
