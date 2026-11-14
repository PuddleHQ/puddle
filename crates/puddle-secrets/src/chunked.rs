// SPDX-License-Identifier: AGPL-3.0-or-later
//! A store that spreads a long value over several entries of the store underneath.
//!
//! The Windows credential store keeps at most 2560 bytes per entry, which is 1280 UTF-16 code
//! units: a longer token (an AWS session token, a long JWT) would be refused there and work
//! elsewhere. [`ChunkedStore`] hides that: a value that fits is stored as it is, a longer one goes
//! in pieces of at most [`CHUNK_UNITS`] code units under ids made from the entry's own, and the
//! entry itself then holds only a small head that names them. Reading puts the pieces back
//! together, so callers see one value of any length.
//!
//! A replacement writes the new pieces under a fresh generation before it swaps the head, so an
//! entry that fails half-way still holds its old value, and the old pieces are removed after.

use crate::name::StoredId;
use crate::secret::Secret;
use crate::store::{SecretStore, StoreError};

/// The most UTF-16 code units in one entry: 2000 bytes as the Windows store holds them, under its
/// 2560-byte limit.
pub const CHUNK_UNITS: usize = 1000;

/// The most pieces one value has. A value of 8192 characters needs at most 17.
const MAX_PIECES: usize = 64;

/// What a head starts with. A value that starts the same way is stored in pieces too, so a head is
/// never taken for a value.
const HEAD_PREFIX: &str = "puddle-chunks:1:";

/// Wraps the store underneath.
#[derive(Debug, Default)]
pub struct ChunkedStore<S> {
    inner: S,
}

impl<S: SecretStore> ChunkedStore<S> {
    /// A store over `inner`.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
}

/// What a head says: which generation's pieces make the value, and how many.
struct Head {
    generation: String,
    pieces: usize,
}

impl Head {
    fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(HEAD_PREFIX)?;
        let (generation, pieces) = rest.split_once(':')?;
        let pieces: usize = pieces.parse().ok()?;
        let valid = !generation.is_empty()
            && generation.bytes().all(|b| b.is_ascii_hexdigit())
            && (1..=MAX_PIECES).contains(&pieces);
        valid.then(|| Self {
            generation: generation.to_owned(),
            pieces,
        })
    }

    fn text(&self) -> String {
        format!("{HEAD_PREFIX}{}:{}", self.generation, self.pieces)
    }

    fn piece(&self, id: &StoredId, index: usize) -> Result<StoredId, StoreError> {
        StoredId::new(format!("{id}-{}-{index}", self.generation)).map_err(|_| StoreError)
    }
}

/// `value` cut into pieces of at most [`CHUNK_UNITS`] UTF-16 code units, at character boundaries.
fn pieces_of(value: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (at, c) in value.char_indices() {
        if units + c.len_utf16() > CHUNK_UNITS {
            pieces.push(&value[start..at]);
            (start, units) = (at, 0);
        }
        units += c.len_utf16();
    }
    pieces.push(&value[start..]);
    pieces
}

fn generation() -> Result<String, StoreError> {
    let mut bytes = [0_u8; 4];
    getrandom::fill(&mut bytes).map_err(|_| StoreError)?;
    Ok(bytes.iter().fold(String::new(), |mut hex, b| {
        use std::fmt::Write as _;
        // writing into a `String` cannot fail
        let _ = write!(hex, "{b:02x}");
        hex
    }))
}

impl<S: SecretStore> ChunkedStore<S> {
    /// The head of the entry under `id`, if it is one.
    fn head(&self, id: &StoredId) -> Result<Option<Head>, StoreError> {
        Ok(self
            .inner
            .get(id)?
            .and_then(|stored| Head::parse(stored.expose())))
    }

    /// Removes the pieces `head` names, as far as they are there.
    fn drop_pieces(&self, id: &StoredId, head: &Head) {
        for index in 0..head.pieces {
            if let Ok(piece) = head.piece(id, index) {
                // A piece that cannot be removed is an orphan nothing refers to; the entry's
                // own removal or the next write is not stopped by it.
                let _ = self.inner.delete(&piece);
            }
        }
    }
}

impl<S: SecretStore> SecretStore for ChunkedStore<S> {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        let Some(stored) = self.inner.get(id)? else {
            return Ok(None);
        };
        let Some(head) = Head::parse(stored.expose()) else {
            return Ok(Some(stored));
        };
        // Room for every piece up front: a `String` that grows frees each smaller copy unwiped.
        let mut value = String::with_capacity(head.pieces * CHUNK_UNITS * 3);
        for index in 0..head.pieces {
            // A missing piece is a damaged entry, never a shorter value.
            let piece = self.inner.get(&head.piece(id, index)?)?.ok_or(StoreError)?;
            value.push_str(piece.expose());
        }
        Ok(Some(Secret::new(value)))
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        let value = secret.expose();
        let before = self.head(id)?;
        let pieces = pieces_of(value);
        if pieces.len() == 1 && !value.starts_with(HEAD_PREFIX) {
            self.inner.set(id, secret)?;
        } else {
            if pieces.len() > MAX_PIECES {
                return Err(StoreError);
            }
            let head = Head {
                generation: generation()?,
                pieces: pieces.len(),
            };
            for (index, piece) in pieces.iter().enumerate() {
                let written = self
                    .inner
                    .set(&head.piece(id, index)?, &Secret::new((*piece).to_owned()));
                if let Err(err) = written {
                    self.drop_pieces(id, &head);
                    return Err(err);
                }
            }
            if let Err(err) = self.inner.set(id, &Secret::new(head.text())) {
                self.drop_pieces(id, &head);
                return Err(err);
            }
        }
        if let Some(old) = before {
            self.drop_pieces(id, &old);
        }
        Ok(())
    }

    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        self.inner.keep_local(id)?;
        if let Some(head) = self.head(id)? {
            for index in 0..head.pieces {
                self.inner.keep_local(&head.piece(id, index)?)?;
            }
        }
        Ok(())
    }

    fn keep_all_local(&self) -> Result<usize, StoreError> {
        self.inner.keep_all_local()
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        let before = self.head(id)?;
        self.inner.delete(id)?;
        if let Some(old) = before {
            self.drop_pieces(id, &old);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
