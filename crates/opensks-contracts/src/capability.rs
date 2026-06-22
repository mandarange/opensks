//! Machine-readable runtime capability registry (recovery release).
//!
//! The product invariant is: the UI must never present a `foundation` /
//! `simulation` surface as if it were `live`. To make that invariant
//! *enforceable* instead of a prose promise in `docs/runtime-truth-matrix.md`,
//! every coding-agent capability declares a [`CapabilityMaturity`] together with
//! the evidence that justifies it. A generated markdown matrix and a CI/test
//! drift gate keep the document and the code from diverging.
//!
//! This module owns the DTOs and the *honest baseline* report for the current
//! commit. As live adapters land, the baseline is replaced by a report computed
//! from real registry/health state — but the rule never changes: a capability
//! may only claim `Live` when it carries supporting evidence.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Schema id for [`RuntimeCapability`].
pub const RUNTIME_CAPABILITY_SCHEMA: &str = "opensks.runtime-capability.v1";
/// Schema id for [`RuntimeCapabilityReport`].
pub const RUNTIME_CAPABILITY_REPORT_SCHEMA: &str = "opensks.runtime-capability-report.v1";

/// How real a capability is at runtime. Ordered from most to least dependable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMaturity {
    /// A real, end-to-end implementation whose user-visible action performs the
    /// real side effect. Requires non-empty `evidence_refs`.
    Live,
    /// Live but operating with a known limitation (e.g. reduced concurrency, a
    /// legacy transport still in the path). Requires non-empty `evidence_refs`.
    Degraded,
    /// Code/types/persistence exist but the vertical path is not yet wired to a
    /// real side effect. Must not be shown as live.
    Foundation,
    /// Deterministic stand-in that mimics success without doing the real work.
    /// Must be visually distinct from live in the UI.
    Simulation,
    /// Not implemented or not currently usable (missing setup/credentials).
    Unavailable,
}

impl CapabilityMaturity {
    /// Whether a user may rely on this capability to perform its real action.
    pub fn is_dependable(self) -> bool {
        matches!(self, Self::Live | Self::Degraded)
    }

    /// Live/Degraded capabilities must justify themselves with evidence.
    pub fn requires_evidence(self) -> bool {
        matches!(self, Self::Live | Self::Degraded)
    }

    /// The vocabulary the app surfaces to users (recovery directive §18.3). The
    /// raw maturity name is never shown directly.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Live => "Available",
            Self::Degraded => "Limited",
            Self::Foundation => "Needs setup",
            Self::Simulation => "Simulation",
            Self::Unavailable => "Unavailable",
        }
    }
}

/// A single declared runtime capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeCapability {
    pub schema: String,
    /// Stable dotted id, e.g. `agent.code_edit`.
    pub id: String,
    /// Short human-facing surface name, e.g. `Chat code edit`.
    pub title: String,
    pub maturity: CapabilityMaturity,
    /// Whether the capability can be relied on right now. Mirrors
    /// `maturity.is_dependable()` for the baseline but is carried explicitly so
    /// a live report can mark a dependable capability temporarily unavailable
    /// (e.g. unhealthy provider) without changing its maturity.
    pub available: bool,
    /// Stable machine reason for the current maturity/availability.
    pub reason_code: String,
    /// Evidence backing the maturity claim (provider/adapter/contract refs).
    pub evidence_refs: Vec<String>,
    /// Actions the UI may offer when the capability is not dependable.
    pub actions: Vec<String>,
}

impl RuntimeCapability {
    /// Validate the internal honesty invariants. Returns the offending reason on
    /// failure so tests/CI can report it.
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("capability id is empty".to_string());
        }
        if self.reason_code.trim().is_empty() {
            return Err(format!("capability `{}` has empty reason_code", self.id));
        }
        if self.maturity.requires_evidence() && self.evidence_refs.is_empty() {
            return Err(format!(
                "capability `{}` claims {:?} but carries no evidence_refs",
                self.id, self.maturity
            ));
        }
        if self.available && !self.maturity.is_dependable() {
            return Err(format!(
                "capability `{}` is marked available but maturity {:?} is not dependable",
                self.id, self.maturity
            ));
        }
        Ok(())
    }
}

/// The full capability report emitted by `opensks capability report --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeCapabilityReport {
    pub schema: String,
    /// Free-form revision marker the report was generated against. The baseline
    /// report leaves this `None`; a live report stamps it with the resolved
    /// commit/build so a release proof is reproducible.
    pub generated_for: Option<String>,
    pub capabilities: Vec<RuntimeCapability>,
}

impl RuntimeCapabilityReport {
    /// Validate every capability and reject duplicate ids.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for cap in &self.capabilities {
            cap.validate()?;
            if !seen.insert(cap.id.as_str()) {
                return Err(format!("duplicate capability id `{}`", cap.id));
            }
        }
        Ok(())
    }

    /// Render the deterministic markdown truth matrix. The output is the source
    /// of `docs/runtime-truth-matrix.generated.md`; a drift check keeps the
    /// committed file in sync with this code.
    pub fn render_truth_matrix_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Runtime Truth Matrix (generated)\n\n");
        out.push_str(
            "<!-- GENERATED FILE — do not edit by hand.\n     Regenerate with: cargo run -p xtask -- capability-matrix\n     Source of truth: opensks_contracts::baseline_capability_report() -->\n\n",
        );
        out.push_str(
            "Each coding-agent capability declares how real it is at runtime. The app must never\n",
        );
        out.push_str(
            "present a `Foundation`/`Simulation` surface as if it were `Live` (recovery directive §18).\n\n",
        );
        out.push_str(
            "| Capability | Surface | Maturity | User label | Available | Reason | Evidence |\n",
        );
        out.push_str("|---|---|---|---|:--:|---|---|\n");
        for cap in &self.capabilities {
            let maturity = format!("{:?}", cap.maturity);
            let available = if cap.available { "yes" } else { "no" };
            let evidence = if cap.evidence_refs.is_empty() {
                "—".to_string()
            } else {
                cap.evidence_refs.join(", ")
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | `{}` | {} |\n",
                cap.id,
                cap.title,
                maturity,
                cap.maturity.display_label(),
                available,
                cap.reason_code,
                evidence,
            ));
        }
        out
    }
}

fn capability(
    id: &str,
    title: &str,
    maturity: CapabilityMaturity,
    reason_code: &str,
    evidence_refs: &[&str],
    actions: &[&str],
) -> RuntimeCapability {
    RuntimeCapability {
        schema: RUNTIME_CAPABILITY_SCHEMA.to_string(),
        id: id.to_string(),
        title: title.to_string(),
        maturity,
        available: maturity.is_dependable(),
        reason_code: reason_code.to_string(),
        evidence_refs: evidence_refs.iter().map(|s| s.to_string()).collect(),
        actions: actions.iter().map(|s| s.to_string()).collect(),
    }
}

/// The honest baseline capability report for the current code. Maturities are
/// grounded in the source-level audit (`docs/baselines/a4fd5a2-audit.md`): the
/// Chat → real-model → real-code-edit path is still a deterministic simulation,
/// while the safe-file, conversation persistence, and reviewed Git paths are
/// real. This is intentionally conservative — a capability is only `Live` when
/// the verified evidence supports it.
pub fn baseline_capability_report() -> RuntimeCapabilityReport {
    use CapabilityMaturity::*;
    RuntimeCapabilityReport {
        schema: RUNTIME_CAPABILITY_REPORT_SCHEMA.to_string(),
        generated_for: None,
        capabilities: vec![
            capability(
                "chat.answer",
                "Chat assistant answer",
                Foundation,
                "real_answer_path_needs_model_configured",
                &[
                    "crate:opensks-adapter",
                    "test:live_openrouter_returns_real_text",
                ],
                &["connect_model"],
            ),
            capability(
                "agent.code_edit",
                "Chat code edit",
                Simulation,
                "deterministic_worker_no_real_edits",
                &[],
                &["connect_model"],
            ),
            capability(
                "agent.parallel_build",
                "Parallel subcontract build",
                Foundation,
                "scheduler_present_but_sync_deterministic_worker",
                &[],
                &["connect_model"],
            ),
            capability(
                "model.dispatch",
                "Model provider dispatch",
                Foundation,
                "openrouter_adapter_present_needs_api_key",
                &["crate:opensks-adapter", "adapter:openrouter"],
                &["connect_model"],
            ),
            capability(
                "agent.local_test_edit",
                "Local test agent file edit",
                Live,
                "deterministic_adapter_performs_real_file_io",
                &[
                    "crate:opensks-adapter",
                    "test:local_test_adapter_really_edits_a_file_on_disk",
                ],
                &[],
            ),
            capability(
                "image.generate",
                "Image generation",
                Foundation,
                "fake_image_model_no_adapter",
                &[],
                &["connect_image_model"],
            ),
            capability(
                "web.research",
                "Web research tool",
                Unavailable,
                "no_web_tool_implementation",
                &[],
                &[],
            ),
            capability(
                "conversation.persistence",
                "Conversation persistence",
                Live,
                "durable_sqlite_repository",
                &["crate:opensks-conversation", "table:conversations"],
                &[],
            ),
            capability(
                "file.edit_manual",
                "Manual file editing",
                Live,
                "safe_file_service_with_optimistic_concurrency",
                &["crate:opensks-file-service", "schema:save-text-result"],
                &[],
            ),
            capability(
                "git.commit",
                "Git commit",
                Live,
                "reviewed_index_hash_commit_path",
                &["crate:opensks-git-service", "schema:git-commit"],
                &[],
            ),
            capability(
                "git.push",
                "Git push",
                Live,
                "protected_push_approval_outbox",
                &["crate:opensks-git-service", "schema:git-isolation"],
                &["approve_push"],
            ),
            capability(
                "stream.protocol",
                "Engine stream protocol",
                Degraded,
                "swift_quiet_window_still_in_product_path",
                &["crate:opensks-stream", "schema:engine-stream-frame"],
                &[],
            ),
            capability(
                "pipeline.graph",
                "Live pipeline graph",
                Foundation,
                "projection_present_no_ingest_or_edges",
                &[],
                &[],
            ),
            capability(
                "design.generation",
                "Design system generation",
                Foundation,
                "studio_scaffold_without_persist_compile_apply",
                &[],
                &[],
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_report_is_internally_valid() {
        let report = baseline_capability_report();
        report.validate().expect("baseline report must be valid");
    }

    #[test]
    fn live_capabilities_carry_evidence() {
        for cap in baseline_capability_report().capabilities {
            if cap.maturity.requires_evidence() {
                assert!(
                    !cap.evidence_refs.is_empty(),
                    "{} claims {:?} without evidence",
                    cap.id,
                    cap.maturity
                );
            }
        }
    }

    #[test]
    fn no_simulation_is_marked_available() {
        for cap in baseline_capability_report().capabilities {
            if !cap.maturity.is_dependable() {
                assert!(!cap.available, "{} must not be available", cap.id);
            }
        }
    }

    #[test]
    fn matrix_markdown_is_deterministic_and_nonempty() {
        let report = baseline_capability_report();
        let a = report.render_truth_matrix_markdown();
        let b = report.render_truth_matrix_markdown();
        assert_eq!(a, b);
        assert!(a.contains("| `agent.code_edit` |"));
        assert!(a.contains("Simulation"));
    }

    #[test]
    fn report_round_trips_through_json() {
        let report = baseline_capability_report();
        let json = serde_json::to_string(&report).unwrap();
        let parsed: RuntimeCapabilityReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, parsed);
    }
}
