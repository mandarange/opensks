use opensks_contracts::{TerminalExecutionDecision, TerminalRiskLevel};
use opensks_terminal::classify_command_risk;

#[test]
fn destructive_commands_require_approval() {
    let decision = classify_command_risk("rm -rf .");
    assert_eq!(decision.risk, TerminalRiskLevel::Destructive);
    assert_eq!(
        decision.decision,
        TerminalExecutionDecision::RequireApproval
    );
    assert!(decision.requires_approval);
}

#[test]
fn secret_exposure_commands_require_approval() {
    let decision = classify_command_risk("cat .env");
    assert_eq!(decision.risk, TerminalRiskLevel::SecretExposure);
    assert_eq!(
        decision.decision,
        TerminalExecutionDecision::RequireApproval
    );
    assert!(decision.requires_approval);
}

#[test]
fn safe_known_commands_are_allowed() {
    let decision = classify_command_risk("cargo test -p opensks-terminal");
    assert_eq!(decision.risk, TerminalRiskLevel::Safe);
    assert_eq!(decision.decision, TerminalExecutionDecision::Allow);
    assert!(!decision.requires_approval);
}
