//! Process diagnostic contract (recovery release §19.2, §25.4).
//!
//! When a child process (daemon or external adapter/tool) exits abnormally, a
//! [`ProcessDiagnostic`] carries a secret-redacted, bounded summary suitable for
//! a "View diagnostics" affordance — never raw stderr or credentials.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PROCESS_DIAGNOSTIC_SCHEMA: &str = "opensks.process-diagnostic.v1";

/// A redacted post-mortem for a child process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProcessDiagnostic {
    pub schema: String,
    pub process_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Bounded (≤4096 lines / ≤1MB), secret-redacted tail of stderr.
    pub stderr_tail_redacted: String,
    pub reason_code: String,
    pub occurred_at_ms: u64,
}

impl ProcessDiagnostic {
    /// Whether the process ended cleanly (exit code 0, no signal).
    pub fn is_clean_exit(&self) -> bool {
        self.signal.is_none() && self.exit_code == Some(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_round_trips() {
        let diag = ProcessDiagnostic {
            schema: PROCESS_DIAGNOSTIC_SCHEMA.to_string(),
            process_label: "daemon".to_string(),
            exit_code: Some(101),
            signal: None,
            stderr_tail_redacted: "panic: ...".to_string(),
            reason_code: "daemon_crash".to_string(),
            occurred_at_ms: 1000,
        };
        let json = serde_json::to_string(&diag).unwrap();
        let parsed: ProcessDiagnostic = serde_json::from_str(&json).unwrap();
        assert_eq!(diag, parsed);
        assert!(!diag.is_clean_exit());
    }
}
