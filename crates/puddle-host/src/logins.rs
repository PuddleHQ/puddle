// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's side of captured logins: one [`Logins`] for each start of a workspace's sandbox, the
//! credential store its real tokens are kept in, and the notice that goes to the user when a
//! login cannot be captured.
//!
//! The capture itself is the `puddle-logins` crate's, and the swap is the proxy's. What belongs
//! here is the wiring: whether the workspace has capture on (its setting, read when it starts),
//! the registry of stand-ins the workspace's termination swaps from (shared with everything else
//! that makes stand-ins), the hosts the workspace decrypts for its logins, and the deletion of
//! the real tokens with the workspace.

use std::path::PathBuf;
use std::sync::Arc;

use puddle_logins::{LoginNotices, Logins, Marker, Problem, Profile, builtin};
use puddle_proxy::StandIns;
use puddle_secrets::SecretStore;
use puddle_settings::resolve;
use puddle_types::{Event, EventSink, LoginProblemKind, WorkspaceId};

use crate::settings_read::{self, SettingsReadError};

/// Turns what `puddle-logins` reports into the event the user's screens show a notice for.
struct EventNotices {
    events: Arc<dyn EventSink>,
}

impl LoginNotices for EventNotices {
    fn problem(&self, workspace: &WorkspaceId, profile: &Profile, problem: Problem) {
        self.events.emit(Event::LoginProblem {
            workspace: workspace.clone(),
            service: profile.name.to_owned(),
            kind: kind_of(problem),
        });
    }
}

/// The wire name of a problem. A problem this version has no name for is `Unknown`: it says
/// nothing about the login that may be untrue.
fn kind_of(problem: Problem) -> LoginProblemKind {
    match problem {
        Problem::StoreUnavailable => LoginProblemKind::StoreUnavailable,
        Problem::UnusableToken => LoginProblemKind::UnusableToken,
        Problem::BoundToken => LoginProblemKind::BoundToken,
        Problem::UnnamedToken => LoginProblemKind::UnnamedToken,
        Problem::Unreadable => LoginProblemKind::Unreadable,
        Problem::CaptureOff => LoginProblemKind::CaptureOff,
        Problem::UnexpectedAnswer => LoginProblemKind::UnexpectedAnswer,
        // A problem a later version adds.
        _ => LoginProblemKind::Unknown,
    }
}

/// What makes a workspace's [`Logins`].
#[derive(Clone)]
pub(crate) struct LoginKeeper {
    /// Where the real tokens are kept: the operating system's credential store.
    store: Arc<dyn SecretStore>,
    events: Arc<dyn EventSink>,
    /// Where the markers are. Without it every start with capture off asks the store.
    markers: Option<PathBuf>,
}

impl LoginKeeper {
    pub(crate) fn new(store: Arc<dyn SecretStore>, events: Arc<dyn EventSink>) -> Self {
        Self {
            store,
            events,
            markers: None,
        }
    }

    /// The same with each workspace's marker in a file of `dir`.
    pub(crate) fn with_markers(mut self, dir: PathBuf) -> Self {
        self.markers = Some(dir);
        self
    }

    /// The captured logins of `workspace` for this start, swapping from `registry`. With
    /// `enabled` false nothing is captured and nothing is decrypted for logins.
    /// The marker of `workspace`: none when the keeper was given no folder.
    fn marker(&self, workspace: &WorkspaceId) -> Marker {
        self.markers
            .as_deref()
            .map_or_else(Marker::none, |dir| Marker::for_workspace(dir, workspace))
    }

    pub(crate) fn logins(
        &self,
        workspace: &WorkspaceId,
        enabled: bool,
        registry: Arc<StandIns>,
    ) -> Logins {
        Logins::new(
            workspace.clone(),
            enabled,
            builtin(),
            registry,
            Arc::clone(&self.store),
            Arc::new(EventNotices {
                events: Arc::clone(&self.events),
            }),
            self.marker(workspace),
        )
    }

    /// Deletes every token kept for a deleted `workspace`. The workspace is gone either way: a
    /// token that stays behind is unreachable (nothing names it any more) and is only logged.
    pub(crate) async fn delete_workspace(&self, workspace: &WorkspaceId) {
        let (store, name) = (Arc::clone(&self.store), workspace.clone());
        let failed =
            tokio::task::spawn_blocking(move || Logins::delete_workspace(&store, &name, builtin()))
                .await
                .unwrap_or(usize::MAX);
        self.marker(workspace).clear();
        if failed > 0 {
            tracing::warn!(%workspace, failed, "a deleted workspace's saved logins could not be removed from the credential store");
        }
    }
}

/// Whether `workspace` captures logins: its own setting, else the global one, else on. Settings
/// that cannot be read count as on (the safer reading: Puddle keeps the token), with the reason
/// logged.
pub(crate) fn capture_enabled(
    settings: &dyn puddle_api::SettingsRepo,
    workspace: &WorkspaceId,
) -> bool {
    let read = || -> Result<bool, SettingsReadError> {
        let global = settings_read::global(settings)?;
        let own = settings_read::workspace(settings, workspace)?;
        Ok(resolve(&global, Some(&own)).capture_logins.value)
    };
    read().unwrap_or_else(|reason| {
        tracing::warn!(%workspace, %reason, "login capture stays on: the settings cannot be read");
        true
    })
}

/// Whether `workspace` warns about a credential Puddle did not add: its own setting, else the
/// global one, else on. Settings that cannot be read count as on (the safer reading), with the
/// reason logged.
pub(crate) fn foreign_warning_enabled(
    settings: &dyn puddle_api::SettingsRepo,
    workspace: &WorkspaceId,
) -> bool {
    let read = || -> Result<bool, SettingsReadError> {
        let global = settings_read::global(settings)?;
        let own = settings_read::workspace(settings, workspace)?;
        Ok(resolve(&global, Some(&own)).warn_foreign_credentials.value)
    };
    read().unwrap_or_else(|reason| {
        tracing::warn!(%workspace, %reason, "the credential warning stays on: the settings cannot be read");
        true
    })
}

#[cfg(test)]
mod tests;
