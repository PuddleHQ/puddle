// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspace disks the workspace list does not name, on the wire.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A workspace disk Puddle keeps whose workspace its list does not name. Puddle cannot tell from
/// outside whether the disk holds any files, so none of this says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrphanVolumeView {
    /// The id a restore names; also the name of the workspace a restore makes.
    pub id: String,
    /// The disk's name in the runtime, `ws-` and the id.
    pub volume: String,
}

/// The workspace disks the list does not name, in id order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrphanVolumeList {
    /// Empty when the list names every workspace disk.
    pub volumes: Vec<OrphanVolumeView>,
}

/// `POST /api/orphan-volumes/{id}/restore`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreOrphanRequest {
    /// The address of the repository on the disk, used only when Puddle cannot read one from the
    /// remote of a checkout there. An HTTPS address; an SSH one is refused as in a create.
    #[serde(default)]
    pub repository: Option<String>,
}
