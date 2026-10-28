// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on the method and path of a request on the wire.

use puddle_types::WorkspaceName;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A rule on method and path. It only refuses: the requests it names to a host Puddle decrypts
/// are answered with a 403 and never leave the computer. The host is always decrypted for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RequestRule {
    /// Row id, never reused. Numbered apart from the rules on hosts.
    pub id: i64,
    /// The workspace it applies to; `null` for every workspace.
    #[schema(required = true)]
    pub workspace: Option<WorkspaceName>,
    /// The host pattern: `example.com` or `*.example.com`.
    pub host: String,
    /// The methods it names, upper case; empty for every method.
    pub methods: Vec<String>,
    /// The path pattern in canonical form: `/a/b`, with `*` for one segment and a closing `/**`
    /// for everything below.
    pub path: String,
    /// Epoch ms.
    pub created_at: u64,
    /// `cli`, `ui` or `api`.
    pub created_by: String,
}

impl From<puddle_store::RequestRule> for RequestRule {
    fn from(rule: puddle_store::RequestRule) -> Self {
        Self {
            id: rule.id,
            workspace: rule.workspace,
            host: rule.host,
            methods: rule.methods.names().to_vec(),
            path: rule.path.to_string(),
            created_at: rule.created_at,
            created_by: rule.created_by.as_str().to_owned(),
        }
    }
}

/// Rules on method and path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RequestRuleList {
    /// In id order.
    pub rules: Vec<RequestRule>,
}

/// A rule on method and path to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRequestRuleRequest {
    /// The workspace the rule applies to; `null` for every workspace.
    #[serde(default)]
    #[schema(required = true)]
    pub workspace: Option<WorkspaceName>,
    /// A host name or `*.` followed by a domain with at least two labels; no IP address.
    pub host: String,
    /// HTTP methods (letters and digits, case does not matter); left out or empty for every
    /// method. At most 16.
    #[serde(default)]
    pub methods: Vec<String>,
    /// The path: `/` followed by segments, where `*` is one whole segment and a closing `/**` is
    /// everything below. No query string. It is compared after percent-decoding, with `.` and
    /// `..` resolved and case ignored.
    pub path: String,
}
