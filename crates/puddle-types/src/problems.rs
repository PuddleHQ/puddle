// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background problems the user should hear about: something Puddle does on its own (a sweep, a
//! clean-up, a start-up step) failed, and nobody asked for it, so no request can carry the error.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::{Event, EventSink, WorkspaceId};

/// One background problem, as `GET /api/problems` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Problem {
    /// Names the problem; raising it again replaces the old one, and it ends with
    /// [`Problems::clear`].
    pub key: String,
    /// The workspace it is about, or `null` when it is about Puddle.
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(required = true))]
    pub workspace: Option<WorkspaceId>,
    /// What went wrong, in one line. It can quote tool output: escape it when rendering.
    pub title: String,
    /// What Puddle did about it and what the user can do. It can quote tool output: escape it
    /// when rendering.
    pub detail: String,
}

impl Problem {
    /// A problem about Puddle.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            workspace: None,
            title: title.into(),
            detail: detail.into(),
        }
    }

    /// The same problem, about `workspace`.
    #[must_use]
    pub fn about(mut self, workspace: WorkspaceId) -> Self {
        self.workspace = Some(workspace);
        self
    }
}

/// The problems that stand now. A client reads them with `GET /api/problems` and rereads on
/// [`Event::ProblemsChanged`], so one that came before the client connected is not missed.
pub struct Problems {
    open: Mutex<BTreeMap<String, Problem>>,
    events: Arc<dyn EventSink>,
}

impl std::fmt::Debug for Problems {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Problems").finish_non_exhaustive()
    }
}

impl Problems {
    /// An empty list that announces its changes to `events`.
    #[must_use]
    pub fn new(events: Arc<dyn EventSink>) -> Self {
        Self {
            open: Mutex::default(),
            events,
        }
    }

    /// Raises `problem`, replacing one with the same key, and logs it. Announces a change only
    /// when the list differs, so a problem that repeats every minute is not announced every
    /// minute.
    pub fn raise(&self, problem: Problem) {
        tracing::warn!(key = %problem.key, title = %problem.title, detail = %problem.detail, "problem raised");
        let changed = {
            let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            let changed = open.get(&problem.key) != Some(&problem);
            if changed {
                open.insert(problem.key.clone(), problem);
            }
            changed
        };
        if changed {
            self.events.emit(Event::ProblemsChanged {});
        }
    }

    /// Ends the problem `key` (its cause went away); does nothing when there is none.
    pub fn clear(&self, key: &str) {
        let removed = self
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key)
            .is_some();
        if removed {
            self.events.emit(Event::ProblemsChanged {});
        }
    }

    /// Ends every problem whose key starts with `prefix` (for example all of a workspace's).
    pub fn clear_prefix(&self, prefix: &str) {
        let removed = {
            let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            let before = open.len();
            open.retain(|key, _| !key.starts_with(prefix));
            open.len() != before
        };
        if removed {
            self.events.emit(Event::ProblemsChanged {});
        }
    }

    /// The problems that stand, ordered by key.
    #[must_use]
    pub fn list(&self) -> Vec<Problem> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests;
