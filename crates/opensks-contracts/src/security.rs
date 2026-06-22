//! Structured security report contract (PR-044).
//!
//! These snake_case wire DTOs are the portable, git-trackable shape emitted by
//! `opensks security report` and consumed by the audit gate. The report carries
//! only structured findings, severity rollups, and named check results — never
//! raw secret material or machine-absolute paths. The audit gate is a pure
//! function of this report: it fails when any `critical`/`high` finding is still
//! `open`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const SECURITY_REPORT_SCHEMA: &str = "opensks.security-report.v1";

/// Severity of a single finding, highest to lowest. `critical` and `high` gate
/// the audit when their status is `open`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// True for severities that block the audit gate when left open.
    pub fn is_gating(&self) -> bool {
        matches!(self, Self::Critical | Self::High)
    }
}

/// Lifecycle status of a finding. Only `open` gating findings fail the audit;
/// `accepted` records an explicit, owned risk decision and `fixed` is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Accepted,
    Fixed,
}

impl FindingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Accepted => "accepted",
            Self::Fixed => "fixed",
        }
    }
}

/// A single security finding. `owner` and `deadline` are optional and only set
/// for `accepted`/tracked risks; they are plain strings (e.g. an ISO date) and
/// never carry secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecurityFinding {
    pub id: String,
    pub severity: Severity,
    pub category: String,
    pub title: String,
    pub detail: String,
    pub status: FindingStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

/// Count of findings by severity. A deterministic rollup of `findings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct SeveritySummary {
    pub critical: u32,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
}

/// A named built-in check and whether it passed. Checks are the cheap,
/// deterministic posture signals (redaction enabled, capabilities configured,
/// approval/replay present, dependency advisories scanned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecurityCheck {
    pub name: String,
    pub passed: bool,
}

/// The full structured security report. `generated_at` is an RFC3339-style
/// timestamp string supplied by the caller (no implicit clock in the contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecurityReport {
    pub schema: String,
    pub generated_at: String,
    #[serde(default)]
    pub findings: Vec<SecurityFinding>,
    pub summary: SeveritySummary,
    #[serde(default)]
    pub checks: Vec<SecurityCheck>,
}

impl SecurityReport {
    /// Build a report from findings + checks, deriving `summary` deterministically
    /// from the findings so the rollup can never drift from the list.
    pub fn new(
        generated_at: impl Into<String>,
        findings: Vec<SecurityFinding>,
        checks: Vec<SecurityCheck>,
    ) -> Self {
        let summary = Self::summarize(&findings);
        Self {
            schema: SECURITY_REPORT_SCHEMA.to_string(),
            generated_at: generated_at.into(),
            findings,
            summary,
            checks,
        }
    }

    /// Roll up a severity histogram from a finding list.
    pub fn summarize(findings: &[SecurityFinding]) -> SeveritySummary {
        let mut summary = SeveritySummary::default();
        for finding in findings {
            match finding.severity {
                Severity::Critical => summary.critical += 1,
                Severity::High => summary.high += 1,
                Severity::Medium => summary.medium += 1,
                Severity::Low => summary.low += 1,
            }
        }
        summary
    }

    /// True iff any gating (`critical`/`high`) finding is still `open`. The
    /// `audit` gate fails when this is true.
    pub fn has_open_blocking_finding(&self) -> bool {
        self.findings
            .iter()
            .any(|f| f.severity.is_gating() && matches!(f.status, FindingStatus::Open))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, severity: Severity, status: FindingStatus) -> SecurityFinding {
        SecurityFinding {
            id: id.to_string(),
            severity,
            category: "test".to_string(),
            title: "t".to_string(),
            detail: "d".to_string(),
            status,
            owner: None,
            deadline: None,
        }
    }

    #[test]
    fn summary_is_derived_from_findings() {
        let report = SecurityReport::new(
            "2026-06-22T00:00:00Z",
            vec![
                finding("a", Severity::Critical, FindingStatus::Open),
                finding("b", Severity::High, FindingStatus::Fixed),
                finding("c", Severity::Low, FindingStatus::Accepted),
            ],
            vec![SecurityCheck {
                name: "redaction_enabled".to_string(),
                passed: true,
            }],
        );
        assert_eq!(report.summary.critical, 1);
        assert_eq!(report.summary.high, 1);
        assert_eq!(report.summary.low, 1);
        assert_eq!(report.summary.medium, 0);
    }

    #[test]
    fn open_blocking_finding_gates_audit() {
        let blocking = SecurityReport::new(
            "t",
            vec![finding("a", Severity::High, FindingStatus::Open)],
            vec![],
        );
        assert!(blocking.has_open_blocking_finding());

        // Accepted/fixed gating findings do not block; open low/medium do not block.
        let clean = SecurityReport::new(
            "t",
            vec![
                finding("a", Severity::Critical, FindingStatus::Accepted),
                finding("b", Severity::High, FindingStatus::Fixed),
                finding("c", Severity::Medium, FindingStatus::Open),
                finding("d", Severity::Low, FindingStatus::Open),
            ],
            vec![],
        );
        assert!(!clean.has_open_blocking_finding());
    }

    #[test]
    fn report_roundtrips_through_json() {
        let report = SecurityReport::new(
            "2026-06-22T00:00:00Z",
            vec![SecurityFinding {
                id: "advisory-posture".to_string(),
                severity: Severity::Medium,
                category: "dependencies".to_string(),
                title: "External dependency advisory posture".to_string(),
                detail: "cargo-deny / cargo-audit scan dependencies in CI.".to_string(),
                status: FindingStatus::Accepted,
                owner: Some("security".to_string()),
                deadline: Some("2026-12-31".to_string()),
            }],
            vec![SecurityCheck {
                name: "dependency_advisories_scanned".to_string(),
                passed: true,
            }],
        );
        let json = serde_json::to_string(&report).expect("serialize report");
        assert!(json.contains("\"schema\":\"opensks.security-report.v1\""));
        assert!(json.contains("\"severity\":\"medium\""));
        assert!(json.contains("\"status\":\"accepted\""));
        let decoded: SecurityReport = serde_json::from_str(&json).expect("decode report");
        assert_eq!(decoded, report);
    }
}
