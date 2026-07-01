use opensks_contracts::{
    COMPLETION_PROOF_SCHEMA, CompletionProof, CoverageSeal, ProofClaim, TrustStatus,
};

pub fn build_completion_proof(
    id: impl Into<String>,
    run_id: impl Into<String>,
    generated_at: impl Into<String>,
    claims: Vec<ProofClaim>,
    required_requirement_ids: Vec<String>,
    covered_requirement_ids: Vec<String>,
    evidence_refs: Vec<String>,
) -> CompletionProof {
    let uncovered_requirement_ids: Vec<String> = required_requirement_ids
        .iter()
        .filter(|required| !covered_requirement_ids.contains(required))
        .cloned()
        .collect();
    let all_claims_verified = !claims.is_empty()
        && claims
            .iter()
            .all(|claim| claim.status == TrustStatus::Verified);
    let has_required_requirements = !required_requirement_ids.is_empty();
    let final_seal_allowed =
        has_required_requirements && uncovered_requirement_ids.is_empty() && all_claims_verified;
    CompletionProof {
        schema: COMPLETION_PROOF_SCHEMA.to_string(),
        id: id.into(),
        run_id: run_id.into(),
        status: if final_seal_allowed {
            TrustStatus::Verified
        } else {
            TrustStatus::NotVerified
        },
        claims,
        coverage: CoverageSeal {
            required: required_requirement_ids.len() as u32,
            covered: covered_requirement_ids.len() as u32,
            uncovered_requirement_ids,
            final_seal_allowed,
        },
        evidence_refs,
        generated_at: generated_at.into(),
    }
}

pub fn claim(
    id: impl Into<String>,
    text: impl Into<String>,
    status: TrustStatus,
    evidence_refs: Vec<String>,
) -> ProofClaim {
    ProofClaim {
        id: id.into(),
        text: text.into(),
        status,
        evidence_refs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncovered_requirement_blocks_final_seal() {
        let proof = build_completion_proof(
            "proof1",
            "run1",
            "now",
            vec![claim(
                "c1",
                "implemented",
                TrustStatus::Verified,
                vec!["test-ref".to_string()],
            )],
            vec!["REQ-1".to_string(), "REQ-2".to_string()],
            vec!["REQ-1".to_string()],
            vec!["test-ref".to_string()],
        );
        assert_eq!(proof.status, TrustStatus::NotVerified);
        assert!(!proof.coverage.final_seal_allowed);
        assert_eq!(proof.coverage.uncovered_requirement_ids, vec!["REQ-2"]);
    }

    #[test]
    fn empty_inputs_do_not_yield_vacuous_final_seal() {
        let proof = build_completion_proof("proof2", "run2", "now", vec![], vec![], vec![], vec![]);
        assert_eq!(proof.status, TrustStatus::NotVerified);
        assert!(!proof.coverage.final_seal_allowed);
    }
}
