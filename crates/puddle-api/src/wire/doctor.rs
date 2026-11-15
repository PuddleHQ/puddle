// SPDX-License-Identifier: AGPL-3.0-or-later
//! The system check on the wire: the report of `puddle doctor` with every field always present
//! (ADR 0002), so the UI's types say `string | null`, never "maybe missing". The command line's
//! JSON leaves out what is empty; this one carries it as `null` and adds each check's title.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How one check came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    /// Fine.
    Ok,
    /// Worth knowing; Puddle works.
    Info,
    /// Puddle works, with a limitation the text explains.
    Warn,
    /// Puddle can't run workspaces until this is fixed.
    Fail,
    /// Not checked, because an earlier check failed or it doesn't apply.
    Skipped,
}

impl From<puddle_doctor::Status> for DoctorStatus {
    fn from(status: puddle_doctor::Status) -> Self {
        match status {
            puddle_doctor::Status::Ok => Self::Ok,
            puddle_doctor::Status::Info => Self::Info,
            puddle_doctor::Status::Warn => Self::Warn,
            puddle_doctor::Status::Fail => Self::Fail,
            puddle_doctor::Status::Skipped => Self::Skipped,
        }
    }
}

/// One line of the system check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorCheck {
    /// Which check, as a stable `snake_case` code (`virtualization`, `hypervisor`, `runtime`,
    /// `test_boot`, ...). A newer Puddle may add codes: show an unknown one by its title.
    pub id: String,
    /// The check's name for people.
    pub title: String,
    /// How it came out.
    pub status: DoctorStatus,
    /// One line: what was found.
    pub summary: String,
    /// What a check that isn't ok found, as a stable `snake_case` code; `null` when ok.
    #[schema(required = true)]
    pub finding: Option<String>,
    /// The exact fix (for a failure) or what a limitation means (for a warning); `null` when
    /// there is nothing to do.
    #[schema(required = true)]
    pub fix: Option<String>,
    /// Raw evidence: an OS error, the runtime's last output lines; `null` when there is none.
    #[schema(required = true)]
    pub detail: Option<String>,
}

/// What the system check found on this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport {
    /// The version of this layout (the command line's `--json` carries the same number).
    pub schema_version: u32,
    /// The Puddle version that ran the checks.
    pub puddle_version: String,
    /// `windows`, `linux`, `macos` or `other`.
    pub os: String,
    /// The CPU architecture.
    pub arch: String,
    /// `true` when no check failed.
    pub ok: bool,
    /// The checks, in a fixed order.
    pub checks: Vec<DoctorCheck>,
    /// How long the checks took, in milliseconds.
    pub elapsed_ms: u64,
}

/// A `snake_case` code (`CheckId`, `Finding`) as the text its JSON form has. The doctor's codes are
/// unit enum variants, which always serialise to a string; anything else would show as its JSON.
fn code(value: &impl Serialize) -> String {
    let json = serde_json::to_value(value).unwrap_or_default();
    json.as_str()
        .map_or_else(|| json.to_string(), str::to_owned)
}

impl From<&puddle_doctor::Check> for DoctorCheck {
    fn from(check: &puddle_doctor::Check) -> Self {
        Self {
            id: code(&check.id),
            title: check.id.title().to_owned(),
            status: check.status.into(),
            summary: check.summary.clone(),
            finding: check.finding.as_ref().map(code),
            fix: check.fix.clone(),
            detail: check.detail.clone(),
        }
    }
}

impl From<&puddle_doctor::Report> for DoctorReport {
    fn from(report: &puddle_doctor::Report) -> Self {
        Self {
            schema_version: report.schema_version,
            puddle_version: report.puddle_version.clone(),
            os: report.os.clone(),
            arch: report.arch.clone(),
            ok: report.ok,
            checks: report.checks.iter().map(DoctorCheck::from).collect(),
            elapsed_ms: report.elapsed_ms,
        }
    }
}

impl DoctorReport {
    /// The checks that failed: the ones that block the first-run flow.
    pub fn failures(&self) -> impl Iterator<Item = &DoctorCheck> {
        self.checks
            .iter()
            .filter(|check| check.status == DoctorStatus::Fail)
    }
}

#[cfg(test)]
mod tests;
