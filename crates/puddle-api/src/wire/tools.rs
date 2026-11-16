// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace's managed tools on the wire: what its mise files ask for, and the edits of the root
//! file. Every string that came from the guest (a file path, a description, mise's reason) is
//! untrusted text: escape it.

use puddle_types::{ToolName, ToolVersion, ToolsFileHash};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One tool a mise file asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToolEntry {
    /// The tool's name as the file spells it.
    pub name: ToolName,
    /// The version the file asks for: `22`, `lts`, an exact version.
    pub requested: ToolVersion,
    /// Whether mise has what the request resolves to.
    pub installed: bool,
}

/// One mise file of the workspace and the tools in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToolsFile {
    /// The file, relative to the workspace folder: `mise.toml`, `api/.tool-versions`.
    pub path: String,
    /// Whether this is the workspace's own `mise.toml`, the only file the API edits. The others
    /// belong to a repository and are shown as they are.
    pub root: bool,
    /// The tools in it, by name.
    pub tools: Vec<ToolEntry>,
}

/// `GET /api/workspaces/{id}/tools`, and the answer of an edit: the tools of every mise file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToolsListing {
    /// Whether the workspace was running and asked. `false`: this is the last listing of this
    /// run, if any, and `problem` says to start it.
    pub running: bool,
    /// When the listing was made, epoch ms; `null` when there is none.
    #[schema(required = true)]
    pub listed_at: Option<u64>,
    /// Why there is nothing fresh to show (stopped, tools off for the workspace, mise failed);
    /// `null` otherwise.
    #[schema(required = true)]
    pub problem: Option<String>,
    /// The SHA-256 of the root `mise.toml` as listed. Send it back with a change.
    #[schema(required = true)]
    pub sha256: Option<ToolsFileHash>,
    /// The workspace's mise files: the root file first when it has tools, then the checkouts'.
    pub files: Vec<ToolsFile>,
}

/// `PUT /api/workspaces/{id}/tools/{name}`: set a tool's version in the root `mise.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolChangeRequest {
    /// The version to ask for: ASCII letters, digits and `. _ - : / @ +`, starting with a letter
    /// or digit, at most 100 characters. Anything else answers `422`.
    pub version: String,
    /// The `sha256` of the listing the user looked at (64 lower-case hex digits, or `absent`). A
    /// different file answers `409`.
    pub sha256: String,
}

/// One tool mise knows by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegistryTool {
    /// The name to use in `mise.toml`.
    pub name: ToolName,
    /// One line about it; may be empty.
    pub description: String,
}

/// `GET /api/workspaces/{id}/tools/registry`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToolRegistry {
    /// The tools by name; empty with `problem` set when mise could not say.
    pub tools: Vec<RegistryTool>,
    /// Why the list is empty, from mise.
    #[schema(required = true)]
    pub problem: Option<String>,
}

/// `GET /api/workspaces/{id}/tools/{name}/versions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ToolVersions {
    /// The newest versions (at most 200), newest first; empty with `reason` set when mise could
    /// not list them (a deny rule on its version host, say). The UI then offers a text box.
    pub versions: Vec<ToolVersion>,
    /// Why the list is empty, from mise.
    #[schema(required = true)]
    pub reason: Option<String>,
}
