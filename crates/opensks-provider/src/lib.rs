use std::collections::BTreeMap;

use opensks_contracts::{
    CapabilityRequirements, HealthState, MODEL_PROFILE_SCHEMA, ModelCapabilities, ModelLimits,
    ModelProfile, ModelRejection, ModelRole, ProviderDescriptor, RoleScore, RoutingDecision,
    RoutingStatus, SecretlessConfigRef,
};
use opensks_policy::{PermissionPolicy, PermissionScope};
use thiserror::Error;

pub const ROUTING_DECISION_SCHEMA: &str = opensks_contracts::ROUTING_DECISION_SCHEMA;
pub const PROVIDER_DESCRIPTOR_SCHEMA: &str = opensks_contracts::PROVIDER_DESCRIPTOR_SCHEMA;

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("model `{0}` is not registered")]
    UnknownModel(String),
    #[error("provider policy denied dispatch: {0}")]
    PolicyDenied(String),
}

#[derive(Debug, Clone)]
pub struct RoutingRequest {
    pub id: String,
    pub role: ModelRole,
    pub required: CapabilityRequirements,
    pub explicit_model_id: Option<String>,
    pub budget_allowed: bool,
}

impl RoutingRequest {
    pub fn for_code(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: ModelRole::Code,
            required: CapabilityRequirements::code(),
            explicit_model_id: None,
            budget_allowed: true,
        }
    }

    pub fn for_image(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            role: ModelRole::Image,
            required: CapabilityRequirements::image_output(),
            explicit_model_id: None,
            budget_allowed: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelRegistry {
    models: Vec<ModelProfile>,
    policy: PermissionPolicy,
}

impl ModelRegistry {
    pub fn new(models: Vec<ModelProfile>, policy: PermissionPolicy) -> Self {
        Self { models, policy }
    }

    pub fn models(&self) -> &[ModelProfile] {
        &self.models
    }

    pub fn snapshot_hash(&self) -> String {
        let mut normalized = self.models.clone();
        normalized.sort_by(|left, right| left.id.cmp(&right.id));
        stable_hash(
            serde_json::to_string(&normalized)
                .unwrap_or_else(|_| "unserializable-model-registry".to_string())
                .as_bytes(),
        )
    }

    pub fn route(&self, request: &RoutingRequest) -> RoutingDecision {
        let permission = self.policy.decide(PermissionScope::ProviderCall);
        if !permission.allowed {
            return RoutingDecision {
                schema: ROUTING_DECISION_SCHEMA.to_string(),
                id: request.id.clone(),
                status: RoutingStatus::BlockedPolicy,
                selected_model_id: None,
                reason_code: permission.reason_code,
                eligible_model_ids: Vec::new(),
                rejected_models: Vec::new(),
                model_snapshot_hash: self.snapshot_hash(),
            };
        }

        if let Some(model_id) = &request.explicit_model_id {
            return self.route_explicit(model_id, request);
        }

        let mut eligible = Vec::new();
        let mut rejected = Vec::new();
        for model in &self.models {
            match eligibility_reason(model, request) {
                None => eligible.push(model),
                Some(reason_code) => rejected.push(ModelRejection {
                    model_id: model.id.clone(),
                    reason_code: reason_code.to_string(),
                }),
            }
        }

        if eligible.is_empty() {
            let status = if rejected
                .iter()
                .any(|item| item.reason_code == "model_disabled")
            {
                RoutingStatus::BlockedDisabled
            } else if rejected
                .iter()
                .any(|item| item.reason_code == "provider_health_open")
            {
                RoutingStatus::BlockedProviderHealth
            } else {
                RoutingStatus::BlockedMissingCapability
            };
            return RoutingDecision {
                schema: ROUTING_DECISION_SCHEMA.to_string(),
                id: request.id.clone(),
                status,
                selected_model_id: None,
                reason_code: "blocked_no_enabled_compatible_model".to_string(),
                eligible_model_ids: Vec::new(),
                rejected_models: rejected,
                model_snapshot_hash: self.snapshot_hash(),
            };
        }

        let selected = if eligible.len() == 1 {
            eligible[0]
        } else {
            eligible
                .iter()
                .max_by(|left, right| {
                    score_model(left, &request.role)
                        .partial_cmp(&score_model(right, &request.role))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| right.id.cmp(&left.id))
                })
                .copied()
                .expect("eligible model")
        };

        RoutingDecision {
            schema: ROUTING_DECISION_SCHEMA.to_string(),
            id: request.id.clone(),
            status: RoutingStatus::Routed,
            selected_model_id: Some(selected.id.clone()),
            reason_code: if eligible.len() == 1 {
                "single_enabled_compatible_model".to_string()
            } else {
                "highest_weighted_role_score".to_string()
            },
            eligible_model_ids: eligible.iter().map(|model| model.id.clone()).collect(),
            rejected_models: rejected,
            model_snapshot_hash: self.snapshot_hash(),
        }
    }

    fn route_explicit(&self, model_id: &str, request: &RoutingRequest) -> RoutingDecision {
        let mut rejected = Vec::new();
        let selected = self.models.iter().find(|model| model.id == model_id);
        if let Some(model) = selected {
            if let Some(reason_code) = eligibility_reason(model, request) {
                rejected.push(ModelRejection {
                    model_id: model.id.clone(),
                    reason_code: reason_code.to_string(),
                });
                return RoutingDecision {
                    schema: ROUTING_DECISION_SCHEMA.to_string(),
                    id: request.id.clone(),
                    status: match reason_code {
                        "model_disabled" => RoutingStatus::BlockedDisabled,
                        "provider_health_open" => RoutingStatus::BlockedProviderHealth,
                        _ => RoutingStatus::BlockedMissingCapability,
                    },
                    selected_model_id: None,
                    reason_code: format!("explicit_model_{reason_code}"),
                    eligible_model_ids: Vec::new(),
                    rejected_models: rejected,
                    model_snapshot_hash: self.snapshot_hash(),
                };
            }
            return RoutingDecision {
                schema: ROUTING_DECISION_SCHEMA.to_string(),
                id: request.id.clone(),
                status: RoutingStatus::Routed,
                selected_model_id: Some(model.id.clone()),
                reason_code: "explicit_model_pin".to_string(),
                eligible_model_ids: vec![model.id.clone()],
                rejected_models: rejected,
                model_snapshot_hash: self.snapshot_hash(),
            };
        }

        RoutingDecision {
            schema: ROUTING_DECISION_SCHEMA.to_string(),
            id: request.id.clone(),
            status: RoutingStatus::BlockedMissingCapability,
            selected_model_id: None,
            reason_code: "explicit_model_not_registered".to_string(),
            eligible_model_ids: Vec::new(),
            rejected_models: vec![ModelRejection {
                model_id: model_id.to_string(),
                reason_code: "not_registered".to_string(),
            }],
            model_snapshot_hash: self.snapshot_hash(),
        }
    }
}

pub fn fake_text_provider_descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        schema: PROVIDER_DESCRIPTOR_SCHEMA.to_string(),
        id: "fake-local".to_string(),
        display_name: "Fake Local Provider".to_string(),
        enabled: true,
        capabilities: ModelCapabilities::text_code(),
        health: HealthState::Healthy,
        config_ref: SecretlessConfigRef {
            source: "testkit".to_string(),
            reference: "fake-provider-no-secret".to_string(),
        },
        secret_value_exposed: false,
    }
}

pub fn fake_text_model(id: impl Into<String>, enabled: bool) -> ModelProfile {
    let mut role_scores = BTreeMap::new();
    role_scores.insert(
        ModelRole::Code,
        RoleScore {
            score: 0.82,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    role_scores.insert(
        ModelRole::Planning,
        RoleScore {
            score: 0.74,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    ModelProfile {
        schema: MODEL_PROFILE_SCHEMA.to_string(),
        id: id.into(),
        provider_id: "fake-local".to_string(),
        display_name: "Fake Text Model".to_string(),
        enabled,
        capabilities: ModelCapabilities::text_code(),
        limits: ModelLimits {
            max_input_tokens: Some(32_000),
            max_output_tokens: Some(8_000),
            requests_per_minute: Some(60),
            tokens_per_minute: None,
            max_concurrency: Some(4),
        },
        pricing: None,
        health: HealthState::Healthy,
        role_scores,
        user_tags: vec!["deterministic".to_string(), "fake".to_string()],
        config_ref: SecretlessConfigRef {
            source: "testkit".to_string(),
            reference: "fake-model-no-secret".to_string(),
        },
    }
}

pub fn fake_image_model(id: impl Into<String>, enabled: bool) -> ModelProfile {
    let mut model = fake_text_model(id, enabled);
    model.display_name = "Fake Image Model".to_string();
    model.capabilities = ModelCapabilities::image();
    model.role_scores.clear();
    model.role_scores.insert(
        ModelRole::Image,
        RoleScore {
            score: 0.9,
            evidence_refs: vec!["fake-provider-deterministic-profile".to_string()],
        },
    );
    model
}

fn eligibility_reason(model: &ModelProfile, request: &RoutingRequest) -> Option<&'static str> {
    if !model.enabled {
        return Some("model_disabled");
    }
    if matches!(
        model.health,
        HealthState::Unavailable | HealthState::OpenCircuit
    ) {
        return Some("provider_health_open");
    }
    if !request.budget_allowed {
        return Some("budget_denied");
    }
    if !model.capabilities.satisfies(&request.required) {
        return Some("capability_mismatch");
    }
    None
}

fn score_model(model: &ModelProfile, role: &ModelRole) -> f64 {
    let role_quality = model
        .role_scores
        .get(role)
        .or_else(|| model.role_scores.get(&ModelRole::General))
        .map(|score| score.score)
        .unwrap_or(0.5);
    let reliability = match model.health {
        HealthState::Healthy => 1.0,
        HealthState::Degraded => 0.55,
        HealthState::Unknown => 0.4,
        HealthState::Unavailable | HealthState::OpenCircuit => 0.0,
    };
    let structured = if model.capabilities.structured_output {
        1.0
    } else {
        0.0
    };
    role_quality * 0.34 + reliability * 0.20 + structured * 0.08
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_model_is_never_routed() {
        let registry = ModelRegistry::new(
            vec![fake_text_model("disabled-code", false)],
            PermissionPolicy::default(),
        );
        let decision = registry.route(&RoutingRequest::for_code("route-disabled"));
        assert_eq!(decision.status, RoutingStatus::BlockedDisabled);
        assert!(decision.selected_model_id.is_none());
        assert_eq!(decision.rejected_models[0].reason_code, "model_disabled");
    }

    #[test]
    fn image_task_blocks_text_only_model() {
        let registry = ModelRegistry::new(
            vec![fake_text_model("text-only", true)],
            PermissionPolicy::default(),
        );
        let decision = registry.route(&RoutingRequest::for_image("route-image"));
        assert_eq!(decision.status, RoutingStatus::BlockedMissingCapability);
        assert_eq!(
            decision.rejected_models[0].reason_code,
            "capability_mismatch"
        );
    }

    #[test]
    fn single_enabled_compatible_model_fallback_routes() {
        let registry = ModelRegistry::new(
            vec![
                fake_text_model("disabled-code", false),
                fake_text_model("only-code", true),
            ],
            PermissionPolicy::default(),
        );
        let decision = registry.route(&RoutingRequest::for_code("route-code"));
        assert_eq!(decision.status, RoutingStatus::Routed);
        assert_eq!(decision.selected_model_id.as_deref(), Some("only-code"));
        assert_eq!(decision.reason_code, "single_enabled_compatible_model");
    }

    #[test]
    fn unhealthy_provider_is_blocked() {
        let mut model = fake_text_model("bad-health", true);
        model.health = HealthState::OpenCircuit;
        let registry = ModelRegistry::new(vec![model], PermissionPolicy::default());
        let decision = registry.route(&RoutingRequest::for_code("route-health"));
        assert_eq!(decision.status, RoutingStatus::BlockedProviderHealth);
        assert_eq!(
            decision.rejected_models[0].reason_code,
            "provider_health_open"
        );
    }
}
