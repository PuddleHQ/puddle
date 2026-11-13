// SPDX-License-Identifier: GPL-3.0-or-later
//! Consents on the wire.

use puddle_settings as settings;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::ApiError;

// ---------------------------------------------------------------------------------------------
// Consents

/// What the user agreed to or declined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Consent {
    /// Never asked.
    NotAsked,
    /// Agreed.
    Granted {
        /// Epoch ms.
        at: u64,
        /// The terms version shown.
        terms_version: String,
    },
    /// Declined.
    Declined {
        /// Epoch ms.
        at: u64,
        /// The terms version shown.
        terms_version: String,
    },
}

impl From<&settings::Consent> for Consent {
    fn from(c: &settings::Consent) -> Self {
        match c {
            settings::Consent::NotAsked => Self::NotAsked,
            settings::Consent::Granted { at, terms_version } => Self::Granted {
                at: at.0,
                terms_version: terms_version.to_string(),
            },
            settings::Consent::Declined { at, terms_version } => Self::Declined {
                at: at.0,
                terms_version: terms_version.to_string(),
            },
        }
    }
}

/// Every consent Puddle asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Consents {
    /// Usage telemetry.
    pub telemetry: Consent,
    /// Crash reports.
    pub crash_reports: Consent,
    /// Microsoft's VS Code server and its licence terms.
    pub vscode_server: Consent,
}

impl From<&settings::Consents> for Consents {
    fn from(c: &settings::Consents) -> Self {
        Self {
            telemetry: c.get(settings::ConsentKind::Telemetry).into(),
            crash_reports: c.get(settings::ConsentKind::CrashReports).into(),
            vscode_server: c.get(settings::ConsentKind::VsCodeServer).into(),
        }
    }
}

/// Which consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    /// Usage telemetry.
    Telemetry,
    /// Crash reports.
    CrashReports,
    /// Microsoft's VS Code server.
    VscodeServer,
}

impl From<ConsentKind> for settings::ConsentKind {
    fn from(kind: ConsentKind) -> Self {
        match kind {
            ConsentKind::Telemetry => Self::Telemetry,
            ConsentKind::CrashReports => Self::CrashReports,
            ConsentKind::VscodeServer => Self::VsCodeServer,
        }
    }
}

/// The user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConsentDecision {
    /// Agreed.
    Granted,
    /// Declined.
    Declined,
}

/// Records the user's answer to a consent prompt. Puddle stamps the time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsentRequest {
    /// Agreed or declined.
    pub decision: ConsentDecision,
    /// The version of the terms shown (a date or a licence URL; 1 to 256 visible ASCII
    /// characters).
    #[schema(min_length = 1, max_length = 256)]
    pub terms_version: String,
}

impl ConsentRequest {
    pub(crate) fn into_consent(self, now_ms: u64) -> Result<settings::Consent, ApiError> {
        let terms_version =
            settings::TermsVersion::new(&self.terms_version).map_err(ApiError::invalid)?;
        let at = settings::UnixMillis(now_ms);
        Ok(match self.decision {
            ConsentDecision::Granted => settings::Consent::Granted { at, terms_version },
            ConsentDecision::Declined => settings::Consent::Declined { at, terms_version },
        })
    }
}
