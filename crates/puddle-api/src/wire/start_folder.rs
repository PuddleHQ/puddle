// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's start folder on the wire: where new terminals land and what the editor opens.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::workspaces as domain;

/// `PUT /api/workspaces/{id}/start-folder`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StartFolderRequest {
    /// The folder, relative to the workspace's volume: parts joined by `/`, none empty, `.` or
    /// `..`, no control character or `\`, not inside `.puddle` or `lost+found`, at most 255
    /// bytes. It may name a folder that does not exist yet. `null` is the volume root. Required,
    /// so an empty body never resets it.
    #[serde(deserialize_with = "Option::deserialize")]
    #[schema(required = true)]
    pub path: Option<String>,
}

impl StartFolderRequest {
    pub(crate) fn into_start_folder(self) -> Result<Option<domain::StartFolder>, ApiError> {
        self.path
            .as_deref()
            .map(domain::StartFolder::parse)
            .transpose()
            .map_err(ApiError::invalid)
    }
}

/// The start folder after a change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StartFolderSet {
    /// The start folder now; `null` is the volume root.
    #[schema(required = true)]
    pub start_folder: Option<String>,
    /// Whether the folder exists in the workspace: `true` or `false` while it runs, `null` when it
    /// is stopped (Puddle looks when it starts) or the start folder is the volume root. A folder
    /// that is missing opens the volume root and shows a problem until it exists.
    #[schema(required = true)]
    pub exists: Option<bool>,
}

impl From<domain::StartFolderChange> for StartFolderSet {
    fn from(change: domain::StartFolderChange) -> Self {
        Self {
            start_folder: change.record.start_folder.map(|f| f.as_str().to_owned()),
            exists: change.exists,
        }
    }
}
