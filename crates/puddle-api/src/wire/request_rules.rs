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
    /// Set when the rule's host is on a never-decrypt list the rule applies under (the global
    /// one, and the workspace's own or, for a global rule, any workspace's): the rule decrypts the
    /// host anyway, so a tool there that does not trust Puddle's certificate fails on it. Names
    /// the names it overrides; `null` when it overrides none.
    #[schema(required = true)]
    pub warning: Option<String>,
}

impl RequestRule {
    /// `rule` on the wire; `never` is every never-decrypt name as (workspace, name), the global
    /// ones with no workspace.
    pub(crate) fn new(rule: puddle_store::RequestRule, never: &[(Option<String>, String)]) -> Self {
        let warning = overridden_warning(&rule, never);
        Self {
            id: rule.id,
            workspace: rule.workspace,
            host: rule.host,
            methods: rule.methods.names().to_vec(),
            path: rule.path.to_string(),
            created_at: rule.created_at,
            created_by: rule.created_by.as_str().to_owned(),
            warning,
        }
    }
}

/// Whether two host patterns in stored form (`example.com`, `*.example.com`) name a host in
/// common. `*.example.com` names the names below `example.com`, not `example.com` itself.
fn overlap(a: &str, b: &str) -> bool {
    let below = |name: &str, domain: &str| {
        name.strip_suffix(domain)
            .is_some_and(|label| label.ends_with('.'))
    };
    match (a.strip_prefix("*."), b.strip_prefix("*.")) {
        (None, None) => a == b,
        (Some(domain), None) => below(b, domain),
        (None, Some(domain)) => below(a, domain),
        (Some(x), Some(y)) => x == y || below(x, y) || below(y, x),
    }
}

/// The warning for `rule` when it overrides some of the names in `never` that it applies under.
fn overridden_warning(
    rule: &puddle_store::RequestRule,
    never: &[(Option<String>, String)],
) -> Option<String> {
    let scope = rule.workspace.as_ref().map(WorkspaceName::as_str);
    let names: Vec<String> = never
        .iter()
        .filter(|(workspace, name)| {
            (workspace.is_none() || scope.is_none() || workspace.as_deref() == scope)
                && overlap(&rule.host, name)
        })
        .map(|(workspace, name)| match workspace {
            None => format!("{name} (every workspace)"),
            Some(workspace) => format!("{name} (workspace {workspace})"),
        })
        .collect();
    (!names.is_empty()).then(|| {
        format!(
            "{} is decrypted for this rule although the never-decrypt list names it: {}. A tool there that does not trust Puddle's certificate will fail on it.",
            rule.host,
            names.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::overlap;

    #[test]
    fn patterns_overlap_when_they_name_a_host_in_common() {
        assert!(overlap("api.example.com", "api.example.com"));
        assert!(!overlap("api.example.com", "web.example.com"));
        assert!(overlap("*.example.com", "api.example.com"));
        assert!(overlap("api.example.com", "*.example.com"));
        assert!(
            !overlap("*.example.com", "example.com"),
            "a wildcard is below"
        );
        assert!(!overlap("example.com", "*.example.com"));
        assert!(!overlap("*.example.com", "badexample.com"));
        assert!(overlap("*.example.com", "*.example.com"));
        assert!(overlap("*.example.com", "*.api.example.com"));
        assert!(overlap("*.api.example.com", "*.example.com"));
        assert!(!overlap("*.example.com", "*.example.org"));
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
