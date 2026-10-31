// SPDX-License-Identifier: GPL-3.0-or-later
//! The first-run flow's state on the wire: whether it has been through, and what it shows about
//! the development certificate.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where Puddle's development certificate stands on this computer: the one that makes HTTPS
/// servers in a workspace open in the browser without a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DevCertificate {
    /// Puddle has not looked (no service is wired in this build or state).
    NotChecked,
    /// Windows already trusts an ASP.NET development certificate, so Puddle uses it in every
    /// workspace with no dialog.
    ReusingExisting,
    /// Puddle made its own localhost certificate and Windows trusts it.
    Trusted,
    /// Puddle's own certificate is not trusted yet: Windows asks the user once to confirm.
    NeedsTrust,
    /// The user declined Windows' confirmation. Workspaces still get the certificate; the
    /// browser shows a warning until it is trusted.
    TrustDeclined,
    /// Trusting failed (for example a company policy forbids adding a certificate). Puddle never
    /// overrides a policy.
    TrustFailed,
    /// This operating system has no way to trust the certificate yet.
    Unsupported,
}

/// What pressing "Trust it now" came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DevCertTrustResult {
    /// Where the certificate stands after the attempt.
    pub dev_certificate: DevCertificate,
    /// Why, when the system or Puddle has more to say than the state does (the operating
    /// system's error for `trust_failed`, or that Windows is still waiting for an answer);
    /// `null` otherwise.
    #[schema(required = true)]
    pub detail: Option<String>,
}

/// Where the first-run flow stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FirstRun {
    /// The flow has been finished or skipped, so the app opens on its start screen.
    pub completed: bool,
    /// Epoch ms of that; `null` while it still has to run.
    #[schema(required = true)]
    pub completed_at: Option<u64>,
    /// What the certificates step says about a development certificate.
    pub dev_certificate: DevCertificate,
}

/// Marks the first-run flow finished, or open again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FirstRunRequest {
    /// `true` records that the flow was finished or skipped (the time of the first such call is
    /// kept); `false` makes it run again at the next start.
    pub completed: bool,
}
