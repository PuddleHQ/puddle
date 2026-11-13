// SPDX-License-Identifier: GPL-3.0-or-later
//! Which endpoints still have a request thread nobody waits for.
//!
//! A read on a Windows named pipe cannot be given a time limit without unsafe code, which this
//! crate forbids. A request whose caller gave up therefore stays parked in its read until the agent
//! answers or closes. So that a locked agent does not collect one more thread and handle per
//! request, a Windows host sends no new request to an endpoint while one is parked there.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The number of parked requests per endpoint.
static PARKED: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic elsewhere never leaves these small values half written.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether a request to `path` is parked.
pub(crate) fn is_parked(path: &Path) -> bool {
    lock(&PARKED).as_ref().is_some_and(|s| s.contains_key(path))
}

enum Phase {
    Waiting,
    Parked,
    Finished,
}

/// One request's share of the registry: the caller and the request thread each hold a clone.
#[derive(Clone)]
pub(crate) struct Request {
    path: PathBuf,
    phase: Arc<Mutex<Phase>>,
}

impl Request {
    pub(crate) fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            phase: Arc::new(Mutex::new(Phase::Waiting)),
        }
    }

    /// The caller stopped waiting. A request that already finished parks nothing.
    pub(crate) fn abandoned(&self) {
        let mut phase = lock(&self.phase);
        if matches!(*phase, Phase::Waiting) {
            *phase = Phase::Parked;
            *lock(&PARKED)
                .get_or_insert_with(HashMap::new)
                .entry(self.path.clone())
                .or_default() += 1;
        }
    }

    /// The request thread is ending.
    pub(crate) fn finished(&self) {
        let mut phase = lock(&self.phase);
        if matches!(*phase, Phase::Parked)
            && let Some(counts) = lock(&PARKED).as_mut()
            && let Some(n) = counts.get_mut(&self.path)
        {
            *n -= 1;
            if *n == 0 {
                counts.remove(&self.path);
            }
        }
        *phase = Phase::Finished;
    }
}

#[cfg(test)]
mod tests;
