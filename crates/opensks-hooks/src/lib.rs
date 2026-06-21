use opensks_contracts::{
    HOOK_DECISION_SCHEMA, HOOK_SPEC_SCHEMA, HookAction, HookDecision, HookInvocation, HookPhase,
    HookSpec, Sensitivity,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HookError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct HookEngine {
    hooks: Vec<HookSpec>,
}

impl HookEngine {
    pub fn new(mut hooks: Vec<HookSpec>) -> Self {
        hooks.sort_by(|left, right| {
            left.phase
                .as_order()
                .cmp(&right.phase.as_order())
                .then_with(|| left.order.cmp(&right.order))
                .then_with(|| left.id.cmp(&right.id))
        });
        Self { hooks }
    }

    pub fn dispatch(&self, invocation: &HookInvocation) -> Vec<HookDecision> {
        self.hooks
            .iter()
            .filter(|hook| hook.enabled && hook.phase == invocation.phase)
            .map(|hook| decide(hook, invocation))
            .collect()
    }

    pub fn replay(jsonl: &str) -> Result<Vec<HookDecision>, HookError> {
        jsonl
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(serde_json::from_str)
            .collect::<Result<Vec<_>, _>>()
            .map_err(HookError::from)
    }

    pub fn decisions_jsonl(decisions: &[HookDecision]) -> Result<String, HookError> {
        let mut out = String::new();
        for decision in decisions {
            out.push_str(&serde_json::to_string(decision)?);
            out.push('\n');
        }
        Ok(out)
    }
}

pub fn hook_spec(
    id: impl Into<String>,
    phase: HookPhase,
    order: i32,
    action: HookAction,
) -> HookSpec {
    HookSpec {
        schema: HOOK_SPEC_SCHEMA.to_string(),
        id: id.into(),
        phase,
        order,
        enabled: true,
        timeout_ms: 1_000,
        allow_secret_read: false,
        action,
        reason_code: "configured_hook_outcome".to_string(),
    }
}

pub fn invocation(
    id: impl Into<String>,
    phase: HookPhase,
    payload: serde_json::Value,
) -> HookInvocation {
    HookInvocation {
        schema: "opensks.hook-invocation.v1".to_string(),
        id: id.into(),
        phase,
        run_id: "run-hook".to_string(),
        payload,
        sensitivity: Sensitivity::Internal,
    }
}

fn decide(hook: &HookSpec, invocation: &HookInvocation) -> HookDecision {
    if hook.timeout_ms == 0 {
        return decision(
            hook,
            invocation,
            HookAction::Block,
            "hook_timeout",
            serde_json::json!({"timeout_ms": 0}),
        );
    }
    if !hook.allow_secret_read && contains_secret(&invocation.payload) {
        return decision(
            hook,
            invocation,
            HookAction::Block,
            "hook_secret_access_denied",
            serde_json::json!({"redacted": true}),
        );
    }
    let payload = match hook.action {
        HookAction::Modify => serde_json::json!({
            "modified": true,
            "original_payload_present": !invocation.payload.is_null()
        }),
        HookAction::Redirect => serde_json::json!({
            "redirect_to": "opensks://hook/redirect-target"
        }),
        HookAction::Retry => serde_json::json!({
            "retry_after_ms": hook.timeout_ms.min(1_000)
        }),
        _ => invocation.payload.clone(),
    };
    decision(
        hook,
        invocation,
        hook.action.clone(),
        &hook.reason_code,
        payload,
    )
}

fn decision(
    hook: &HookSpec,
    invocation: &HookInvocation,
    action: HookAction,
    reason_code: &str,
    payload: serde_json::Value,
) -> HookDecision {
    HookDecision {
        schema: HOOK_DECISION_SCHEMA.to_string(),
        invocation_id: invocation.id.clone(),
        hook_id: hook.id.clone(),
        phase: invocation.phase.clone(),
        action,
        reason_code: reason_code.to_string(),
        payload,
        redacted: invocation.sensitivity != Sensitivity::Public,
        evidence_refs: vec!["opensks-hooks:deterministic-dispatch".to_string()],
    }
}

fn contains_secret(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(value) => {
            let lower = value.to_ascii_lowercase();
            lower.contains("secret")
                || lower.contains("token")
                || lower.contains("api_key")
                || lower.contains("sk-")
        }
        serde_json::Value::Array(values) => values.iter().any(contains_secret),
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            let lower = key.to_ascii_lowercase();
            lower.contains("secret")
                || lower.contains("token")
                || lower.contains("api_key")
                || contains_secret(value)
        }),
        _ => false,
    }
}

trait HookPhaseOrder {
    fn as_order(&self) -> u8;
}

impl HookPhaseOrder for HookPhase {
    fn as_order(&self) -> u8 {
        match self {
            Self::BeforeRun => 0,
            Self::BeforeNode => 1,
            Self::BeforeProviderCall => 2,
            Self::BeforeToolCall => 3,
            Self::BeforeApplyPatch => 4,
            Self::BeforeExternalWrite => 5,
            Self::AfterNode => 6,
            Self::AfterRun => 7,
            Self::OnError => 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_execute_in_deterministic_order() {
        let engine = HookEngine::new(vec![
            hook_spec("b", HookPhase::BeforeRun, 2, HookAction::Allow),
            hook_spec("a", HookPhase::BeforeRun, 1, HookAction::Allow),
        ]);
        let decisions = engine.dispatch(&invocation(
            "inv-1",
            HookPhase::BeforeRun,
            serde_json::json!({"task": "ok"}),
        ));
        assert_eq!(decisions[0].hook_id, "a");
        assert_eq!(decisions[1].hook_id, "b");
    }

    #[test]
    fn secret_payload_is_blocked_before_hook_reads_it() {
        let engine = HookEngine::new(vec![hook_spec(
            "secret-guard",
            HookPhase::BeforeProviderCall,
            0,
            HookAction::Allow,
        )]);
        let decisions = engine.dispatch(&invocation(
            "inv-secret",
            HookPhase::BeforeProviderCall,
            serde_json::json!({"api_key": "sk-test"}),
        ));
        assert_eq!(decisions[0].action, HookAction::Block);
        assert_eq!(decisions[0].reason_code, "hook_secret_access_denied");
    }

    #[test]
    fn timeout_blocks_and_replay_is_exact() {
        let mut hook = hook_spec("timeout", HookPhase::BeforeToolCall, 0, HookAction::Retry);
        hook.timeout_ms = 0;
        let engine = HookEngine::new(vec![hook]);
        let decisions = engine.dispatch(&invocation(
            "inv-timeout",
            HookPhase::BeforeToolCall,
            serde_json::json!({}),
        ));
        assert_eq!(decisions[0].action, HookAction::Block);
        let jsonl = HookEngine::decisions_jsonl(&decisions).expect("jsonl");
        let replayed = HookEngine::replay(&jsonl).expect("replay");
        assert_eq!(replayed, decisions);
    }

    #[test]
    fn configured_outcomes_cover_modify_redirect_and_retry() {
        let engine = HookEngine::new(vec![
            hook_spec("modify", HookPhase::BeforeNode, 0, HookAction::Modify),
            hook_spec("redirect", HookPhase::BeforeNode, 1, HookAction::Redirect),
            hook_spec("retry", HookPhase::BeforeNode, 2, HookAction::Retry),
        ]);
        let decisions = engine.dispatch(&invocation(
            "inv-outcomes",
            HookPhase::BeforeNode,
            serde_json::json!({"plain": true}),
        ));
        assert_eq!(decisions[0].action, HookAction::Modify);
        assert_eq!(decisions[1].action, HookAction::Redirect);
        assert_eq!(decisions[2].action, HookAction::Retry);
    }
}
