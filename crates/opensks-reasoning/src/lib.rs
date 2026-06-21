use opensks_contracts::{REASONING_REPORT_SCHEMA, ReasoningClaim, ReasoningReport, TrustStatus};

#[derive(Debug, Clone)]
pub struct DebateInput {
    pub id: String,
    pub max_rounds: u32,
    pub pro_claim: String,
    pub con_counterexample: String,
}

pub fn run_bounded_debate(input: DebateInput) -> ReasoningReport {
    let rounds = input.max_rounds.min(3);
    let unresolved = !input.con_counterexample.trim().is_empty();
    ReasoningReport {
        schema: REASONING_REPORT_SCHEMA.to_string(),
        id: input.id,
        strategy: "bounded_debate".to_string(),
        rounds,
        max_rounds: input.max_rounds,
        status: if unresolved {
            TrustStatus::Partial
        } else {
            TrustStatus::Verified
        },
        claims: vec![
            ReasoningClaim {
                id: "proponent-claim".to_string(),
                role: "proponent".to_string(),
                claim: input.pro_claim,
                evidence: "structured evidence required".to_string(),
                counterexample: String::new(),
            },
            ReasoningClaim {
                id: "opponent-counterexample".to_string(),
                role: "opponent".to_string(),
                claim: "Counterexample review".to_string(),
                evidence: "structured counterexample supplied".to_string(),
                counterexample: input.con_counterexample,
            },
        ],
        hidden_reasoning_persisted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debate_is_bounded_and_keeps_evidence_fields() {
        let report = run_bounded_debate(DebateInput {
            id: "debate".to_string(),
            max_rounds: 9,
            pro_claim: "Change is safe".to_string(),
            con_counterexample: "One unverified path remains".to_string(),
        });
        assert_eq!(report.rounds, 3);
        assert_eq!(report.status, TrustStatus::Partial);
        assert!(!report.hidden_reasoning_persisted);
        assert!(report.claims.iter().all(|claim| !claim.evidence.is_empty()));
        assert!(
            report
                .claims
                .iter()
                .any(|claim| !claim.counterexample.is_empty())
        );
    }
}
