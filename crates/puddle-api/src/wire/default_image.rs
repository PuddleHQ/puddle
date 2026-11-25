// SPDX-License-Identifier: AGPL-3.0-or-later
//! The image a new workspace gets when the form names none.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `GET /api/workspaces/default-image`: the image a create without one boots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DefaultImage {
    /// The image reference, as the create uses it.
    pub image: String,
    /// One line that says what is in the image, read from the image's own labels; `null` while
    /// they are not known yet (the image is still downloading, or its labels say nothing).
    #[schema(required = true)]
    pub description: Option<String>,
}
