// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// The operating system's own error text for `trust_failed`; `null` otherwise.
    #[schema(required = true)]
    pub detail: Option<String>,
    /// Windows' confirmation is still open from an earlier press (or this one ran out of
    /// waiting), so nothing new was asked and `dev_certificate` is the state as it stands.
    pub waiting: bool,
}

/// A step of the first-run flow, in the order the person walks them (the ids of the UI's step list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FirstRunStep {
    /// The welcome screen.
    Welcome,
    /// The system check.
    Check,
    /// The certificates step.
    Certificates,
    /// How to connect.
    Connect,
    /// Which hosts Puddle decrypts.
    Interception,
    /// The look of the app.
    Look,
    /// The last screen.
    Done,
}

impl FirstRunStep {
    /// The id the settings document keeps (the same as on the wire).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Welcome => "welcome",
            Self::Check => "check",
            Self::Certificates => "certificates",
            Self::Connect => "connect",
            Self::Interception => "interception",
            Self::Look => "look",
            Self::Done => "done",
        }
    }
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
    /// The step the person was on when they left the flow, so reopening it resumes there; `null`
    /// before they moved on from the first screen, and once the flow is completed.
    #[schema(required = true)]
    pub step: Option<FirstRunStep>,
    /// What the certificates step says about a development certificate.
    pub dev_certificate: DevCertificate,
}

/// Marks the first-run flow finished or open again, and/or records the step the person is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FirstRunRequest {
    /// `true` records that the flow was finished or skipped (the time of the first such call is
    /// kept); `false` makes it run again at the next start, from the first step. Left out, the
    /// flow's completion is not touched.
    #[serde(default)]
    pub completed: Option<bool>,
    /// The step the person is on, kept so reopening the flow resumes there. Ignored once the flow
    /// is completed.
    #[serde(default)]
    pub step: Option<FirstRunStep>,
}

/// What the first-run banner's Retry came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialStoreRetry {
    /// The credential store took a write on this check, so Puddle can save secrets and sign-ins.
    /// `false` leaves the `credential-store` problem standing with its advice.
    pub working: bool,
}
