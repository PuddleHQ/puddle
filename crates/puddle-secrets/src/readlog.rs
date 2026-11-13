// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the last read of each source came to, kept in memory so a screen can show it without
//! reading again. Only the outcome and the time are kept: never a value. Nothing is written to
//! disk, so after a restart a source has no entry until it is read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use crate::error::SourceError;
use crate::spec::SourceSpec;

/// What asked for the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadUse {
    /// A workspace's request went through the proxy and needed the credential.
    Injection,
    /// Puddle listed a Git host's repositories.
    RepositoryList,
    /// The user pressed Test.
    Test,
}

/// The last read of one source. The value is never part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastRead {
    /// When it happened.
    pub at: SystemTime,
    /// What asked for it.
    pub by: ReadUse,
    /// `None` when the source gave a value, else why not.
    pub problem: Option<SourceError>,
}

/// The sources remembered at most; a source past this is not recorded (the screens then say it is
/// not tested).
const MAX_SOURCES: usize = 1024;

/// The last read of each source. Cheap to share; a call never blocks on a read.
#[derive(Default)]
pub struct ReadLog {
    reads: Mutex<HashMap<SourceSpec, LastRead>>,
    on_change: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for ReadLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadLog").finish_non_exhaustive()
    }
}

impl ReadLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The same, calling `notify` whenever a source's outcome moves (its first read, or a change
    /// between readable, a problem and the problem it is). A repeat of the same outcome is not a
    /// change.
    #[must_use]
    pub fn with_on_change(mut self, notify: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_change = Some(Arc::new(notify));
        self
    }

    /// Notes a read that finished now.
    pub fn record(&self, source: &SourceSpec, by: ReadUse, problem: Option<SourceError>) {
        self.record_at(source, by, problem, SystemTime::now());
    }

    /// Notes a read that finished at `at`.
    pub fn record_at(
        &self,
        source: &SourceSpec,
        by: ReadUse,
        problem: Option<SourceError>,
        at: SystemTime,
    ) {
        let changed = {
            let mut reads = self.reads.lock().unwrap_or_else(PoisonError::into_inner);
            let before = reads.get(source).map(|last| &last.problem);
            let changed = before != Some(&problem);
            if reads.len() >= MAX_SOURCES && !reads.contains_key(source) {
                return;
            }
            reads.insert(source.clone(), LastRead { at, by, problem });
            changed
        };
        if changed && let Some(notify) = &self.on_change {
            notify();
        }
    }

    /// The last read of `source`; `None` before any since Puddle started.
    #[must_use]
    pub fn last(&self, source: &SourceSpec) -> Option<LastRead> {
        self.reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(source)
            .cloned()
    }
}

#[cfg(test)]
mod tests;
