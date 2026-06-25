use std::collections::{BTreeMap, BTreeSet};

use opensks_contracts::{
    CONCURRENCY_DECISION_SCHEMA, CapabilityRequirements, ConcurrencyDecision,
    ConversationTurnSettings, EXECUTION_EVENT_ENVELOPE_SCHEMA, EventKind, ExecutionEventEnvelope,
    Lease, LeaseType, ModelRole, RoutingStatus, SCHEDULER_WORK_ITEM_SCHEMA, SchedulerSnapshot,
    SchedulerWorkItem, Sensitivity, WorkBudget, WorkKind, WorkState,
};
use opensks_event_store::{EventStore, EventStoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

mod mailbox;

pub use mailbox::{CommandMailbox, ControlState, SchedulerCommand};

pub const SCHEDULER_SNAPSHOT_SCHEMA: &str = "opensks.scheduler-snapshot.v1";
pub const DEFAULT_WORKER_LEASE_TTL_MS: u64 = 30_000;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("unknown work item `{0}`")]
    UnknownWorkItem(String),
    #[error("invalid state transition from {from:?} to {to:?}")]
    InvalidTransition { from: WorkState, to: WorkState },
    #[error("work item `{0}` is not ready to lease")]
    WorkItemNotReady(String),
    #[error("work item `{0}` has no active lease")]
    LeaseNotFound(String),
    #[error("lease holder mismatch for `{item_id}`: expected `{expected}`, got `{actual}`")]
    LeaseHolderMismatch {
        item_id: String,
        expected: String,
        actual: String,
    },
    #[error("lease fence mismatch for `{item_id}`: expected `{expected}`, got `{actual}`")]
    LeaseFenceMismatch {
        item_id: String,
        expected: String,
        actual: String,
    },
    #[error("worker batch returned duplicate outcome for `{0}`")]
    DuplicateWorkerOutcome(String),
    #[error("worker batch did not return outcome for `{0}`")]
    MissingWorkerOutcome(String),
    #[error("worker batch returned outcome for unknown work item `{0}`")]
    UnknownWorkerOutcome(String),
    #[error("scheduler made no progress with {0} unfinished items")]
    NoProgress(usize),
    #[error("event store error: {0}")]
    EventStore(#[from] EventStoreError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub requested_workers: u32,
    pub project_max_workers: u32,
    pub provider_max_workers: u32,
    pub per_provider_max_workers: u32,
    pub per_model_max_workers: u32,
    pub worktree_max_workers: u32,
    pub verification_max_workers: u32,
    pub visible_lane_cap: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            requested_workers: 8,
            project_max_workers: 8,
            provider_max_workers: 8,
            per_provider_max_workers: 8,
            per_model_max_workers: 8,
            worktree_max_workers: 8,
            verification_max_workers: 4,
            visible_lane_cap: 6,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DurableScheduler {
    run_id: String,
    items: BTreeMap<String, SchedulerWorkItem>,
    config: SchedulerConfig,
    transitions_committed: u64,
    max_concurrent_workers: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerDispatchOutcome {
    pub work_item_id: String,
    pub worker_id: String,
    pub ok: bool,
    pub message: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerDispatchReport {
    pub run_id: String,
    pub decision: ConcurrencyDecision,
    pub attempted: usize,
    pub completed: usize,
    pub failed: usize,
    #[serde(default)]
    pub max_parallel_batch_size: usize,
    #[serde(default)]
    pub parallel_batches: usize,
    pub worker_ids: Vec<String>,
    pub outcomes: Vec<WorkerDispatchOutcome>,
}

/// Outcome of validating and recording a steer command against a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SteerReceipt {
    /// The target is a known, steerable (non-terminal) work item.
    Applied { target_id: String },
    /// The target is unknown or terminal; carries the reason it was rejected.
    Rejected { target_id: String, reason: String },
}

impl SteerReceipt {
    pub fn is_applied(&self) -> bool {
        matches!(self, SteerReceipt::Applied { .. })
    }
}

/// Report describing how a cancel command enforced on the dispatch path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelReport {
    pub run_id: String,
    pub reason_code: String,
    /// Items that were still non-terminal and got transitioned to `Cancelled`.
    pub cancelled: Vec<String>,
    /// Items that were already `Completed` and left untouched (partial run).
    pub completed: Vec<String>,
}

/// The true control state a dispatch resolved to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionControlState {
    /// Dispatch ran normally to idle.
    Running,
    /// Dispatch was blocked by a pause; the run quiesced to `paused`.
    Paused,
    /// Dispatch was blocked by a cancel; queued work was cancelled.
    Cancelled,
}

/// Result of a control-aware dispatch, reporting the TRUE control state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlledDispatch {
    pub snapshot: SchedulerSnapshot,
    pub report: WorkerDispatchReport,
    pub control_state: ExecutionControlState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel: Option<CancelReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseHeartbeatReport {
    pub run_id: String,
    pub work_item_id: String,
    pub lease_id: String,
    pub holder: String,
    pub heartbeat_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecoveryRecord {
    pub work_item_id: String,
    pub lease_id: String,
    pub holder: String,
    pub state_before: String,
    pub state_after: String,
    pub last_seen_ms: u64,
    pub expires_at_ms: u64,
    pub ttl_ms: u64,
    pub expired: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRecoveryReport {
    pub run_id: String,
    pub checked_at_ms: u64,
    pub active_count: usize,
    pub expired_count: usize,
    pub active: Vec<LeaseRecoveryRecord>,
    pub expired: Vec<LeaseRecoveryRecord>,
}

#[derive(Debug, Clone, Copy)]
struct LeaseFence<'a> {
    holder: &'a str,
    lease_id: &'a str,
}

#[derive(Debug, Clone)]
struct RunningDispatch {
    item_id: String,
    lease_id: String,
    batch_id: String,
    batch_size: usize,
    lane_index: usize,
    worker_context_pack_ref: Option<String>,
    resource_semaphore_bound: bool,
}

#[derive(Debug, Clone)]
pub struct ConversationTurnSchedulerInput<'a> {
    pub run_id: &'a str,
    pub turn_id: &'a str,
    pub project_id: &'a str,
    pub conversation_id: &'a str,
    pub settings: &'a ConversationTurnSettings,
    pub settings_digest: &'a str,
    pub context_pack_ref: Option<&'a str>,
    pub resource_limits: Option<ConversationTurnSchedulerResourceLimits>,
    pub role_plan: Option<ConversationTurnSchedulerRolePlan>,
    pub objective_plan: Option<ConversationTurnObjectivePlan>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationTurnObjectivePlan {
    pub graph_id: String,
    pub plan_hash: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compiled_plan_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_ref: Option<String>,
    pub work_items: Vec<SchedulerWorkItem>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurnSchedulerRoleAssignment {
    pub role: ModelRole,
    pub status: RoutingStatus,
    pub selected_model_id: Option<String>,
    pub provider_id: Option<String>,
    pub reason_code: String,
    pub reused_model: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurnSchedulerRolePlan {
    pub reason_code: String,
    pub assignments: Vec<ConversationTurnSchedulerRoleAssignment>,
    pub distinct_model_count: u32,
    pub reused_model_count: u32,
    pub blocked_role_count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationTurnSchedulerResourceLimits {
    pub provider_max_workers: u32,
    pub per_provider_max_workers: u32,
    pub per_model_max_workers: u32,
}

#[derive(Debug, Clone)]
pub struct ConversationTurnSchedulerBootstrap {
    pub work_item: SchedulerWorkItem,
    pub work_items: Vec<SchedulerWorkItem>,
    pub snapshot: SchedulerSnapshot,
    pub queued_event_sequence: u64,
    pub reused: bool,
}

impl WorkerDispatchReport {
    fn new(run_id: String, decision: ConcurrencyDecision) -> Self {
        Self {
            run_id,
            decision,
            attempted: 0,
            completed: 0,
            failed: 0,
            max_parallel_batch_size: 0,
            parallel_batches: 0,
            worker_ids: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    fn add_worker_id(&mut self, worker_id: &str) {
        if !self.worker_ids.iter().any(|existing| existing == worker_id) {
            self.worker_ids.push(worker_id.to_string());
            self.worker_ids.sort();
        }
    }

    fn merge(&mut self, next: WorkerDispatchReport) {
        self.attempted += next.attempted;
        self.completed += next.completed;
        self.failed += next.failed;
        self.max_parallel_batch_size = self
            .max_parallel_batch_size
            .max(next.max_parallel_batch_size);
        self.parallel_batches += next.parallel_batches;
        for worker_id in next.worker_ids {
            self.add_worker_id(&worker_id);
        }
        self.outcomes.extend(next.outcomes);
    }
}

pub trait WorkerDriver {
    fn acquire_holder(&mut self, item: &SchedulerWorkItem) -> String;
    fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome;

    fn execute_batch(&mut self, items: Vec<SchedulerWorkItem>) -> Vec<WorkerDispatchOutcome> {
        items.into_iter().map(|item| self.execute(&item)).collect()
    }
}

#[derive(Debug, Clone)]
pub struct DeterministicWorker {
    worker_id: String,
}

impl DeterministicWorker {
    pub fn new(worker_id: impl Into<String>) -> Self {
        Self {
            worker_id: worker_id.into(),
        }
    }
}

impl WorkerDriver for DeterministicWorker {
    fn acquire_holder(&mut self, _item: &SchedulerWorkItem) -> String {
        self.worker_id.clone()
    }

    fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome {
        WorkerDispatchOutcome {
            work_item_id: item.id.clone(),
            worker_id: self.worker_id.clone(),
            ok: true,
            message: format!("deterministic worker completed {}", item.id),
            evidence_refs: vec![
                "scheduler:deterministic-worker".to_string(),
                format!("worker:{}:result", self.worker_id),
            ],
        }
    }
}

impl DurableScheduler {
    pub fn new(
        run_id: impl Into<String>,
        items: Vec<SchedulerWorkItem>,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            items: items
                .into_iter()
                .map(|item| (item.id.clone(), item))
                .collect(),
            config,
            transitions_committed: 0,
            max_concurrent_workers: 0,
        }
    }

    pub fn work_items(&self) -> Vec<SchedulerWorkItem> {
        self.items.values().cloned().collect()
    }

    pub fn governor_decision(&self, sampled_at: impl Into<String>) -> ConcurrencyDecision {
        let mut limits = BTreeMap::new();
        limits.insert("project".to_string(), self.config.project_max_workers);
        limits.insert("provider".to_string(), self.config.provider_max_workers);
        limits.insert(
            "per_provider".to_string(),
            self.config.per_provider_max_workers,
        );
        limits.insert("per_model".to_string(), self.config.per_model_max_workers);
        limits.insert("worktree".to_string(), self.config.worktree_max_workers);
        limits.insert(
            "verification".to_string(),
            self.config.verification_max_workers,
        );
        let admitted = self
            .config
            .requested_workers
            .min(self.config.project_max_workers)
            .min(self.config.provider_max_workers)
            .min(self.config.worktree_max_workers)
            .max(1);
        let mut backpressure = Vec::new();
        if admitted < self.config.requested_workers {
            backpressure.push("requested_workers_capped_by_runtime_limits".to_string());
        }
        ConcurrencyDecision {
            schema: CONCURRENCY_DECISION_SCHEMA.to_string(),
            requested: self.config.requested_workers,
            admitted,
            visible_lanes: admitted.min(self.config.visible_lane_cap),
            headless_lanes: admitted.saturating_sub(self.config.visible_lane_cap),
            limits,
            backpressure,
            sampled_at: sampled_at.into(),
        }
    }

    pub fn ready_items(&self) -> Vec<String> {
        let completed: BTreeSet<String> = self
            .items
            .values()
            .filter(|item| item.state == WorkState::Completed)
            .map(|item| item.id.clone())
            .collect();
        let mut ready: Vec<&SchedulerWorkItem> = self
            .items
            .values()
            .filter(|item| item.state == WorkState::Ready)
            .filter(|item| {
                item.dependencies
                    .iter()
                    .all(|dependency| completed.contains(dependency))
            })
            .collect();
        ready.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        ready.into_iter().map(|item| item.id.clone()).collect()
    }

    fn admit_ready_batch(&self, ready: Vec<String>, admitted: usize) -> Vec<String> {
        let mut batch = Vec::with_capacity(admitted);
        let mut provider_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut model_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut admitted_path_scopes = Vec::new();
        let per_provider_limit = self.config.per_provider_max_workers.max(1) as usize;
        let per_model_limit = self.config.per_model_max_workers.max(1) as usize;

        for item_id in ready {
            if batch.len() >= admitted {
                break;
            }
            let Some(item) = self.items.get(&item_id) else {
                continue;
            };
            let provider_key = item
                .provider_selector
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let model_key = item
                .model_selector
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let provider_blocked = provider_key.is_some_and(|provider| {
                provider_counts.get(provider).copied().unwrap_or(0) >= per_provider_limit
            });
            let model_blocked = model_key.is_some_and(|model| {
                model_counts.get(model).copied().unwrap_or(0) >= per_model_limit
            });
            let path_scope_blocked =
                path_scope_conflicts_any(&item.path_scope, &admitted_path_scopes);
            if provider_blocked || model_blocked || path_scope_blocked {
                continue;
            }
            if let Some(provider) = provider_key {
                *provider_counts.entry(provider.to_string()).or_default() += 1;
            }
            if let Some(model) = model_key {
                *model_counts.entry(model.to_string()).or_default() += 1;
            }
            if path_scope_is_explicit(&item.path_scope) {
                admitted_path_scopes.push(item.path_scope.clone());
            }
            batch.push(item_id);
        }
        batch
    }

    pub fn transition(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        to: WorkState,
        evidence_refs: Vec<String>,
    ) -> Result<(), SchedulerError> {
        self.transition_with_payload(store, item_id, to, evidence_refs, Map::new())
    }

    fn transition_with_payload(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        to: WorkState,
        mut evidence_refs: Vec<String>,
        extra_payload: Map<String, Value>,
    ) -> Result<(), SchedulerError> {
        let from = self
            .items
            .get(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?
            .state
            .clone();
        if !is_valid_transition(&from, &to) {
            return Err(SchedulerError::InvalidTransition { from, to });
        }
        let lease = self
            .items
            .get(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?
            .lease
            .clone();
        let path_scope = self
            .items
            .get(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?
            .path_scope
            .clone();
        let mut payload = Map::new();
        payload.insert(
            "work_item_id".to_string(),
            Value::String(item_id.to_string()),
        );
        payload.insert("from".to_string(), Value::String(format!("{from:?}")));
        payload.insert("to".to_string(), Value::String(format!("{to:?}")));
        if let Some(lease) = lease {
            payload.insert("lease_id".to_string(), Value::String(lease.id));
            payload.insert("lease_holder".to_string(), Value::String(lease.holder));
            payload.insert(
                "lease_type".to_string(),
                serde_json::to_value(&lease.lease_type)?,
            );
        }
        for (key, value) in extra_payload {
            payload.insert(key, value);
        }
        append_path_scope_payload(&mut payload, &mut evidence_refs, &path_scope)?;

        let event = ExecutionEventEnvelope {
            schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!(
                "evt-{}-{}-{}",
                self.run_id,
                item_id,
                self.transitions_committed + 1
            ),
            run_id: self.run_id.clone(),
            sequence: 0,
            occurred_at: format!("transition-{}", self.transitions_committed + 1),
            actor: "opensks-scheduler".to_string(),
            causation_id: None,
            correlation_id: Some(item_id.to_string()),
            kind: event_kind_for_state(&to),
            payload: Value::Object(payload),
            sensitivity: Sensitivity::Public,
            evidence_refs: evidence_refs.clone(),
        };
        store.append_event(event)?;
        self.transitions_committed += 1;

        let item = self
            .items
            .get_mut(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?;
        item.state = to;
        for evidence_ref in evidence_refs {
            if !item.evidence_refs.contains(&evidence_ref) {
                item.evidence_refs.push(evidence_ref);
            }
        }
        Ok(())
    }

    pub fn transition_with_fence(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        to: WorkState,
        holder: &str,
        lease_id: &str,
        evidence_refs: Vec<String>,
    ) -> Result<(), SchedulerError> {
        self.transition_with_fenced_payload(
            store,
            item_id,
            to,
            LeaseFence { holder, lease_id },
            evidence_refs,
            worker_payload(holder, None, None),
        )
    }

    fn transition_with_fenced_payload(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        to: WorkState,
        fence: LeaseFence<'_>,
        evidence_refs: Vec<String>,
        mut extra_payload: Map<String, Value>,
    ) -> Result<(), SchedulerError> {
        let lease = self.validate_lease_fence(item_id, fence.holder, fence.lease_id)?;
        extra_payload.insert("fencing_token".to_string(), Value::String(lease.id.clone()));
        extra_payload.insert(
            "fencing_holder".to_string(),
            Value::String(lease.holder.clone()),
        );
        self.transition_with_payload(store, item_id, to, evidence_refs, extra_payload)
    }

    pub fn simulate_until_idle(
        &mut self,
        store: &mut EventStore,
    ) -> Result<SchedulerSnapshot, SchedulerError> {
        let decision = self.governor_decision("deterministic-simulation");
        let admitted = decision.admitted as usize;
        loop {
            let ready = self.ready_items();
            if ready.is_empty() {
                let unfinished = self
                    .items
                    .values()
                    .filter(|item| !item.state.is_terminal())
                    .count();
                if unfinished == 0 {
                    break;
                }
                return Err(SchedulerError::NoProgress(unfinished));
            }

            let batch: Vec<String> = ready.into_iter().take(admitted).collect();
            self.max_concurrent_workers = self.max_concurrent_workers.max(batch.len() as u32);
            for item_id in &batch {
                let lease = self.assign_lease(item_id, "sim-worker")?;
                self.transition(store, item_id, WorkState::Leased, Vec::new())?;
                self.transition_with_fence(
                    store,
                    item_id,
                    WorkState::Running,
                    "sim-worker",
                    &lease.id,
                    Vec::new(),
                )?;
                self.transition_with_fence(
                    store,
                    item_id,
                    WorkState::Completed,
                    "sim-worker",
                    &lease.id,
                    vec!["deterministic-scheduler-simulation".to_string()],
                )?;
            }
        }

        Ok(self.snapshot_with_evidence(decision, vec!["event-store-replay-required".to_string()]))
    }

    pub fn dispatch_ready_batch<D: WorkerDriver>(
        &mut self,
        store: &mut EventStore,
        driver: &mut D,
    ) -> Result<WorkerDispatchReport, SchedulerError> {
        let decision = self.governor_decision("worker-dispatch-batch");
        self.dispatch_ready_batch_with_decision(store, driver, decision)
    }

    pub fn lease_ready_item(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        holder: &str,
    ) -> Result<(), SchedulerError> {
        if !self.ready_items().iter().any(|ready| ready == item_id) {
            return Err(SchedulerError::WorkItemNotReady(item_id.to_string()));
        }
        self.assign_lease(item_id, holder)?;
        self.transition_with_payload(
            store,
            item_id,
            WorkState::Leased,
            vec!["scheduler:worker-lease".to_string()],
            worker_payload(holder, None, None),
        )
    }

    pub fn active_lease(&self, item_id: &str) -> Result<Lease, SchedulerError> {
        self.items
            .get(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?
            .lease
            .clone()
            .ok_or_else(|| SchedulerError::LeaseNotFound(item_id.to_string()))
    }

    fn validate_lease_fence(
        &self,
        item_id: &str,
        holder: &str,
        lease_id: &str,
    ) -> Result<Lease, SchedulerError> {
        let lease = self.active_lease(item_id)?;
        if lease.holder != holder {
            return Err(SchedulerError::LeaseHolderMismatch {
                item_id: item_id.to_string(),
                expected: lease.holder,
                actual: holder.to_string(),
            });
        }
        if lease.id != lease_id {
            return Err(SchedulerError::LeaseFenceMismatch {
                item_id: item_id.to_string(),
                expected: lease.id,
                actual: lease_id.to_string(),
            });
        }
        Ok(lease)
    }

    pub fn heartbeat_lease(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        holder: &str,
        heartbeat_at_ms: u64,
    ) -> Result<LeaseHeartbeatReport, SchedulerError> {
        self.heartbeat_lease_internal(store, item_id, holder, None, heartbeat_at_ms)
    }

    pub fn heartbeat_lease_with_fence(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        holder: &str,
        lease_id: &str,
        heartbeat_at_ms: u64,
    ) -> Result<LeaseHeartbeatReport, SchedulerError> {
        self.heartbeat_lease_internal(store, item_id, holder, Some(lease_id), heartbeat_at_ms)
    }

    fn heartbeat_lease_internal(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        holder: &str,
        lease_id: Option<&str>,
        heartbeat_at_ms: u64,
    ) -> Result<LeaseHeartbeatReport, SchedulerError> {
        let item = self
            .items
            .get(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?;
        let lease = item
            .lease
            .as_ref()
            .ok_or_else(|| SchedulerError::LeaseNotFound(item_id.to_string()))?
            .clone();
        if lease.holder != holder {
            return Err(SchedulerError::LeaseHolderMismatch {
                item_id: item_id.to_string(),
                expected: lease.holder,
                actual: holder.to_string(),
            });
        }
        if let Some(lease_id) = lease_id {
            if lease.id != lease_id {
                return Err(SchedulerError::LeaseFenceMismatch {
                    item_id: item_id.to_string(),
                    expected: lease.id,
                    actual: lease_id.to_string(),
                });
            }
        }
        let expires_at_ms = heartbeat_at_ms.saturating_add(lease.ttl_ms);
        let report = LeaseHeartbeatReport {
            run_id: self.run_id.clone(),
            work_item_id: item_id.to_string(),
            lease_id: lease.id.clone(),
            holder: holder.to_string(),
            heartbeat_at_ms,
            expires_at_ms,
        };
        self.append_lease_lifecycle_event(
            store,
            EventKind::LeaseHeartbeat,
            item_id,
            vec!["scheduler:lease-heartbeat".to_string()],
            lease_lifecycle_payload(&lease, item_id, item.state.clone(), item.state.clone())
                .into_iter()
                .chain([
                    (
                        "heartbeat_at_ms".to_string(),
                        Value::Number(heartbeat_at_ms.into()),
                    ),
                    (
                        "expires_at_ms".to_string(),
                        Value::Number(expires_at_ms.into()),
                    ),
                ])
                .collect(),
        )?;
        let item = self
            .items
            .get_mut(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?;
        if let Some(lease) = item.lease.as_mut() {
            lease.last_heartbeat_at_ms = Some(heartbeat_at_ms);
        }
        if !item
            .evidence_refs
            .contains(&"scheduler:lease-heartbeat".to_string())
        {
            item.evidence_refs
                .push("scheduler:lease-heartbeat".to_string());
        }
        Ok(report)
    }

    pub fn expire_stale_leases(
        &mut self,
        store: &mut EventStore,
        checked_at_ms: u64,
    ) -> Result<LeaseRecoveryReport, SchedulerError> {
        let mut report = LeaseRecoveryReport {
            run_id: self.run_id.clone(),
            checked_at_ms,
            active_count: 0,
            expired_count: 0,
            active: Vec::new(),
            expired: Vec::new(),
        };
        let leased_item_ids: Vec<String> = self
            .items
            .values()
            .filter(|item| item.lease.is_some())
            .filter(|item| !item.state.is_terminal())
            .map(|item| item.id.clone())
            .collect();

        for item_id in leased_item_ids {
            let item = self
                .items
                .get(&item_id)
                .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.clone()))?
                .clone();
            let lease = item
                .lease
                .clone()
                .ok_or_else(|| SchedulerError::LeaseNotFound(item_id.clone()))?;
            let last_seen_ms = lease_last_seen_ms(&lease);
            let expires_at_ms = last_seen_ms.saturating_add(lease.ttl_ms);
            if checked_at_ms <= expires_at_ms {
                report.active.push(LeaseRecoveryRecord {
                    work_item_id: item_id,
                    lease_id: lease.id,
                    holder: lease.holder,
                    state_before: format!("{:?}", item.state),
                    state_after: format!("{:?}", item.state),
                    last_seen_ms,
                    expires_at_ms,
                    ttl_ms: lease.ttl_ms,
                    expired: false,
                });
                continue;
            }

            self.append_lease_lifecycle_event(
                store,
                EventKind::LeaseExpired,
                &item_id,
                vec!["scheduler:lease-expired-recovered".to_string()],
                lease_lifecycle_payload(&lease, &item_id, item.state.clone(), WorkState::Ready)
                    .into_iter()
                    .chain([
                        (
                            "last_seen_ms".to_string(),
                            Value::Number(last_seen_ms.into()),
                        ),
                        (
                            "expires_at_ms".to_string(),
                            Value::Number(expires_at_ms.into()),
                        ),
                        (
                            "checked_at_ms".to_string(),
                            Value::Number(checked_at_ms.into()),
                        ),
                        (
                            "expiry_reason".to_string(),
                            Value::String("lease_ttl_elapsed".to_string()),
                        ),
                    ])
                    .collect(),
            )?;
            let expired_record = LeaseRecoveryRecord {
                work_item_id: item_id.clone(),
                lease_id: lease.id,
                holder: lease.holder,
                state_before: format!("{:?}", item.state),
                state_after: format!("{:?}", WorkState::Ready),
                last_seen_ms,
                expires_at_ms,
                ttl_ms: lease.ttl_ms,
                expired: true,
            };
            let item = self
                .items
                .get_mut(&item_id)
                .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.clone()))?;
            item.lease = None;
            item.state = WorkState::Ready;
            if !item
                .evidence_refs
                .contains(&"scheduler:lease-expired-recovered".to_string())
            {
                item.evidence_refs
                    .push("scheduler:lease-expired-recovered".to_string());
            }
            report.expired.push(expired_record);
        }
        report.active_count = report.active.len();
        report.expired_count = report.expired.len();
        Ok(report)
    }

    pub fn dispatch_until_idle<D: WorkerDriver>(
        &mut self,
        store: &mut EventStore,
        driver: &mut D,
    ) -> Result<(SchedulerSnapshot, WorkerDispatchReport), SchedulerError> {
        let decision = self.governor_decision("worker-dispatch");
        let mut report = WorkerDispatchReport::new(self.run_id.clone(), decision.clone());
        loop {
            let ready = self.ready_items();
            if ready.is_empty() {
                let unfinished = self
                    .items
                    .values()
                    .filter(|item| !item.state.is_terminal())
                    .count();
                if unfinished == 0 {
                    break;
                }
                return Err(SchedulerError::NoProgress(unfinished));
            }
            let next = self.dispatch_ready_batch_with_decision(store, driver, decision.clone())?;
            if next.attempted == 0 {
                return Err(SchedulerError::NoProgress(
                    self.items
                        .values()
                        .filter(|item| !item.state.is_terminal())
                        .count(),
                ));
            }
            report.merge(next);
        }

        let mut evidence_refs = vec![
            "event-store-replay-required".to_string(),
            "scheduler:worker-dispatch".to_string(),
        ];
        if self.max_concurrent_workers > 1 {
            evidence_refs.push("scheduler:parallel-batch-dispatch".to_string());
        }
        let snapshot = self.snapshot_with_evidence(decision, evidence_refs);
        Ok((snapshot, report))
    }

    /// Derive the durable control state for this run by replaying its events.
    ///
    /// The control events (cancel / pause / resume / steer) ARE the mailbox, so
    /// this recovers the same intent after a restart from a fresh replay.
    pub fn control_state(&self, store: &EventStore) -> Result<ControlState, SchedulerError> {
        let events = store.replay(&self.run_id)?;
        Ok(ControlState::from_events(&events))
    }

    /// Derive the pending command mailbox for this run from its events.
    pub fn command_mailbox(&self, store: &EventStore) -> Result<CommandMailbox, SchedulerError> {
        let events = store.replay(&self.run_id)?;
        Ok(CommandMailbox::from_events(&events))
    }

    /// Validate a steer target against the run's work items and state.
    ///
    /// Returns [`SteerReceipt::Applied`] when the target is a known, non-terminal
    /// (steerable) work item, otherwise [`SteerReceipt::Rejected`] with a reason.
    pub fn validate_steer_target(&self, target_id: &str) -> SteerReceipt {
        match self.items.get(target_id) {
            None => SteerReceipt::Rejected {
                target_id: target_id.to_string(),
                reason: "unknown_work_item".to_string(),
            },
            Some(item) if item.state.is_terminal() => SteerReceipt::Rejected {
                target_id: target_id.to_string(),
                reason: format!("work_item_terminal:{:?}", item.state),
            },
            Some(_) => SteerReceipt::Applied {
                target_id: target_id.to_string(),
            },
        }
    }

    /// Enforce a cancel command: transition every still non-terminal item to
    /// `Cancelled` with an explicit reason. Already-completed items stay
    /// completed (partial run). New dispatch is blocked by the caller.
    pub fn apply_cancel(
        &mut self,
        store: &mut EventStore,
        reason_code: &str,
    ) -> Result<CancelReport, SchedulerError> {
        let mut report = CancelReport {
            run_id: self.run_id.clone(),
            reason_code: reason_code.to_string(),
            cancelled: Vec::new(),
            completed: Vec::new(),
        };
        let item_ids: Vec<String> = self.items.keys().cloned().collect();
        for item_id in item_ids {
            let state = self
                .items
                .get(&item_id)
                .map(|item| item.state.clone())
                .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.clone()))?;
            if state == WorkState::Completed {
                report.completed.push(item_id);
                continue;
            }
            if state.is_terminal() {
                continue;
            }
            let mut payload = Map::new();
            payload.insert(
                "reason_code".to_string(),
                Value::String(reason_code.to_string()),
            );
            payload.insert(
                "cancel_origin".to_string(),
                Value::String("run_cancel_command".to_string()),
            );
            self.transition_with_payload(
                store,
                &item_id,
                WorkState::Cancelled,
                vec!["scheduler:run-cancel".to_string()],
                payload,
            )?;
            report.cancelled.push(item_id);
        }
        Ok(report)
    }

    /// Dispatch like [`Self::dispatch_until_idle`], but consult the durable
    /// control mailbox first. A prior `Cancel` cancels still-queued work and
    /// dispatches nothing further; a prior `Pause` blocks new dispatch and the
    /// run reports the true `paused` state (the synchronous worker is always
    /// between items here, so quiescence is immediate).
    pub fn dispatch_until_idle_with_control<D: WorkerDriver>(
        &mut self,
        store: &mut EventStore,
        driver: &mut D,
    ) -> Result<ControlledDispatch, SchedulerError> {
        let control = self.control_state(store)?;
        if control.cancelled {
            let reason = control
                .cancel_reason
                .clone()
                .unwrap_or_else(|| "cancelled".to_string());
            let cancel = self.apply_cancel(store, &reason)?;
            let decision = self.governor_decision("worker-dispatch-cancelled");
            let report = WorkerDispatchReport::new(self.run_id.clone(), decision.clone());
            let snapshot =
                self.snapshot_with_evidence(decision, vec!["scheduler:run-cancel".to_string()]);
            return Ok(ControlledDispatch {
                snapshot,
                report,
                control_state: ExecutionControlState::Cancelled,
                cancel: Some(cancel),
            });
        }
        if control.paused {
            let decision = self.governor_decision("worker-dispatch-paused");
            let report = WorkerDispatchReport::new(self.run_id.clone(), decision.clone());
            let snapshot =
                self.snapshot_with_evidence(decision, vec!["scheduler:run-pause".to_string()]);
            return Ok(ControlledDispatch {
                snapshot,
                report,
                control_state: ExecutionControlState::Paused,
                cancel: None,
            });
        }
        let (snapshot, report) = self.dispatch_until_idle(store, driver)?;
        Ok(ControlledDispatch {
            snapshot,
            report,
            control_state: ExecutionControlState::Running,
            cancel: None,
        })
    }

    fn dispatch_ready_batch_with_decision<D: WorkerDriver>(
        &mut self,
        store: &mut EventStore,
        driver: &mut D,
        decision: ConcurrencyDecision,
    ) -> Result<WorkerDispatchReport, SchedulerError> {
        let admitted = decision.admitted as usize;
        let ready = self.ready_items();
        let batch = self.admit_ready_batch(ready, admitted);
        self.max_concurrent_workers = self.max_concurrent_workers.max(batch.len() as u32);
        let mut report = WorkerDispatchReport::new(self.run_id.clone(), decision);
        if batch.is_empty() {
            return Ok(report);
        }
        let batch_size = batch.len();
        let batch_id = format!("batch-{}-{}", self.run_id, self.transitions_committed + 1);
        report.max_parallel_batch_size = batch_size;
        if batch_size > 1 {
            report.parallel_batches = 1;
        }

        let mut running_batch = Vec::with_capacity(batch_size);
        let parallel_evidence_ref = "scheduler:parallel-batch-dispatch".to_string();
        for (lane_index, item_id) in batch.iter().enumerate() {
            let mut item = self
                .items
                .get(item_id)
                .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.clone()))?
                .clone();
            let lane_index = lane_index + 1;
            let worker_context_pack_ref =
                item.context_pack_ref.as_deref().map(|context_pack_ref| {
                    scoped_worker_context_pack_ref(
                        context_pack_ref,
                        item_id,
                        &batch_id,
                        batch_size,
                        lane_index,
                    )
                });
            item.worker_context_pack_ref = worker_context_pack_ref.clone();
            let holder = driver.acquire_holder(&item);
            report.add_worker_id(&holder);
            let lease = self.assign_lease(item_id, &holder)?;
            if let Some(stored_item) = self.items.get_mut(item_id) {
                stored_item.worker_context_pack_ref = worker_context_pack_ref.clone();
            }
            let mut lease_evidence_refs = vec!["scheduler:worker-lease".to_string()];
            let mut dispatch_evidence_refs = vec!["scheduler:worker-dispatch".to_string()];
            let mut running_evidence_refs = vec!["scheduler:worker-running".to_string()];
            let resource_semaphore_bound =
                item.provider_selector.is_some() || item.model_selector.is_some();
            if resource_semaphore_bound {
                lease_evidence_refs.push("scheduler:provider-model-semaphore".to_string());
                dispatch_evidence_refs.push("scheduler:provider-model-semaphore".to_string());
                running_evidence_refs.push("scheduler:provider-model-semaphore".to_string());
            }
            if batch_size > 1 {
                lease_evidence_refs.push(parallel_evidence_ref.clone());
                dispatch_evidence_refs.push(parallel_evidence_ref.clone());
                running_evidence_refs.push(parallel_evidence_ref.clone());
            }
            if worker_context_pack_ref.is_some() {
                lease_evidence_refs.push("context:worker-context-pack".to_string());
                dispatch_evidence_refs.push("context:worker-context-pack".to_string());
                running_evidence_refs.push("context:worker-context-pack".to_string());
            }
            self.transition_with_payload(
                store,
                item_id,
                WorkState::Leased,
                lease_evidence_refs,
                worker_batch_payload(
                    &holder,
                    None,
                    None,
                    &batch_id,
                    batch_size,
                    lane_index,
                    worker_context_pack_ref.as_deref(),
                ),
            )?;
            self.transition_with_fenced_payload(
                store,
                item_id,
                WorkState::Dispatched,
                LeaseFence {
                    holder: &holder,
                    lease_id: &lease.id,
                },
                dispatch_evidence_refs,
                worker_batch_payload(
                    &holder,
                    None,
                    None,
                    &batch_id,
                    batch_size,
                    lane_index,
                    worker_context_pack_ref.as_deref(),
                ),
            )?;
            self.transition_with_fenced_payload(
                store,
                item_id,
                WorkState::Running,
                LeaseFence {
                    holder: &holder,
                    lease_id: &lease.id,
                },
                running_evidence_refs,
                worker_batch_payload(
                    &holder,
                    None,
                    None,
                    &batch_id,
                    batch_size,
                    lane_index,
                    worker_context_pack_ref.as_deref(),
                ),
            )?;

            running_batch.push(RunningDispatch {
                item_id: item_id.clone(),
                lease_id: lease.id,
                batch_id: batch_id.clone(),
                batch_size,
                lane_index,
                worker_context_pack_ref,
                resource_semaphore_bound,
            });
        }

        let mut expected_item_ids = BTreeSet::new();
        let mut running_items = Vec::with_capacity(running_batch.len());
        for running in &running_batch {
            expected_item_ids.insert(running.item_id.clone());
            running_items.push(
                self.items
                    .get(&running.item_id)
                    .ok_or_else(|| SchedulerError::UnknownWorkItem(running.item_id.clone()))?
                    .clone(),
            );
        }
        let outcomes = driver.execute_batch(running_items);
        let mut outcomes_by_item = BTreeMap::new();
        for outcome in outcomes {
            if !expected_item_ids.contains(&outcome.work_item_id) {
                return Err(SchedulerError::UnknownWorkerOutcome(
                    outcome.work_item_id.clone(),
                ));
            }
            if outcomes_by_item.contains_key(&outcome.work_item_id) {
                return Err(SchedulerError::DuplicateWorkerOutcome(
                    outcome.work_item_id.clone(),
                ));
            }
            outcomes_by_item.insert(outcome.work_item_id.clone(), outcome);
        }

        for running in running_batch {
            let mut outcome = outcomes_by_item
                .remove(&running.item_id)
                .ok_or_else(|| SchedulerError::MissingWorkerOutcome(running.item_id.clone()))?;
            report.attempted += 1;
            report.add_worker_id(&outcome.worker_id);
            let mut outcome_evidence_refs = outcome.evidence_refs.clone();
            if !outcome_evidence_refs
                .iter()
                .any(|evidence_ref| evidence_ref == "scheduler:worker-dispatch")
            {
                outcome_evidence_refs.push("scheduler:worker-dispatch".to_string());
            }
            if running.batch_size > 1
                && !outcome_evidence_refs
                    .iter()
                    .any(|evidence_ref| evidence_ref == "scheduler:parallel-batch-dispatch")
            {
                outcome_evidence_refs.push("scheduler:parallel-batch-dispatch".to_string());
            }
            if running.resource_semaphore_bound
                && !outcome_evidence_refs
                    .iter()
                    .any(|evidence_ref| evidence_ref == "scheduler:provider-model-semaphore")
            {
                outcome_evidence_refs.push("scheduler:provider-model-semaphore".to_string());
            }
            outcome.evidence_refs = outcome_evidence_refs.clone();
            if outcome.ok {
                self.transition_with_fenced_payload(
                    store,
                    &running.item_id,
                    WorkState::ResultReceived,
                    LeaseFence {
                        holder: &outcome.worker_id,
                        lease_id: &running.lease_id,
                    },
                    outcome_evidence_refs.clone(),
                    worker_batch_payload(
                        &outcome.worker_id,
                        Some(&outcome.message),
                        Some(true),
                        &running.batch_id,
                        running.batch_size,
                        running.lane_index,
                        running.worker_context_pack_ref.as_deref(),
                    ),
                )?;
                self.transition_with_fenced_payload(
                    store,
                    &running.item_id,
                    WorkState::Verifying,
                    LeaseFence {
                        holder: &outcome.worker_id,
                        lease_id: &running.lease_id,
                    },
                    vec!["scheduler:worker-result-verification".to_string()],
                    worker_batch_payload(
                        &outcome.worker_id,
                        Some(&outcome.message),
                        Some(true),
                        &running.batch_id,
                        running.batch_size,
                        running.lane_index,
                        running.worker_context_pack_ref.as_deref(),
                    ),
                )?;
                self.transition_with_fenced_payload(
                    store,
                    &running.item_id,
                    WorkState::Applying,
                    LeaseFence {
                        holder: &outcome.worker_id,
                        lease_id: &running.lease_id,
                    },
                    vec!["scheduler:worker-result-apply".to_string()],
                    worker_batch_payload(
                        &outcome.worker_id,
                        Some(&outcome.message),
                        Some(true),
                        &running.batch_id,
                        running.batch_size,
                        running.lane_index,
                        running.worker_context_pack_ref.as_deref(),
                    ),
                )?;
                self.transition_with_fenced_payload(
                    store,
                    &running.item_id,
                    WorkState::Completed,
                    LeaseFence {
                        holder: &outcome.worker_id,
                        lease_id: &running.lease_id,
                    },
                    outcome_evidence_refs,
                    worker_batch_payload(
                        &outcome.worker_id,
                        Some(&outcome.message),
                        Some(true),
                        &running.batch_id,
                        running.batch_size,
                        running.lane_index,
                        running.worker_context_pack_ref.as_deref(),
                    ),
                )?;
                report.completed += 1;
            } else {
                self.transition_with_fenced_payload(
                    store,
                    &running.item_id,
                    WorkState::Failed,
                    LeaseFence {
                        holder: &outcome.worker_id,
                        lease_id: &running.lease_id,
                    },
                    outcome_evidence_refs,
                    worker_batch_payload(
                        &outcome.worker_id,
                        Some(&outcome.message),
                        Some(false),
                        &running.batch_id,
                        running.batch_size,
                        running.lane_index,
                        running.worker_context_pack_ref.as_deref(),
                    ),
                )?;
                report.failed += 1;
            }
            report.outcomes.push(outcome);
        }
        Ok(report)
    }

    fn snapshot_with_evidence(
        &self,
        decision: ConcurrencyDecision,
        evidence_refs: Vec<String>,
    ) -> SchedulerSnapshot {
        let overlap_ratio = if self.max_concurrent_workers > 1 {
            1.0
        } else {
            0.0
        };
        SchedulerSnapshot {
            schema: SCHEDULER_SNAPSHOT_SCHEMA.to_string(),
            run_id: self.run_id.clone(),
            work_items: self.work_items(),
            decision,
            overlap_ratio,
            max_concurrent_workers: self.max_concurrent_workers,
            evidence_refs,
        }
    }

    pub fn snapshot(
        &self,
        sampled_at: impl Into<String>,
        evidence_refs: Vec<String>,
    ) -> SchedulerSnapshot {
        self.snapshot_with_evidence(self.governor_decision(sampled_at), evidence_refs)
    }

    fn append_lease_lifecycle_event(
        &mut self,
        store: &mut EventStore,
        kind: EventKind,
        item_id: &str,
        evidence_refs: Vec<String>,
        payload: Map<String, Value>,
    ) -> Result<(), SchedulerError> {
        let event = ExecutionEventEnvelope {
            schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!(
                "evt-{}-{}-{}",
                self.run_id,
                item_id,
                self.transitions_committed + 1
            ),
            run_id: self.run_id.clone(),
            sequence: 0,
            occurred_at: format!("transition-{}", self.transitions_committed + 1),
            actor: "opensks-scheduler".to_string(),
            causation_id: None,
            correlation_id: Some(item_id.to_string()),
            kind,
            payload: Value::Object(payload),
            sensitivity: Sensitivity::Public,
            evidence_refs,
        };
        store.append_event(event)?;
        self.transitions_committed += 1;
        Ok(())
    }

    fn assign_lease(&mut self, item_id: &str, holder: &str) -> Result<Lease, SchedulerError> {
        let acquired_at_ms = self.transitions_committed + 1;
        let lease_type = self
            .items
            .get(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))
            .map(|item| {
                if path_scope_is_explicit(&item.path_scope) {
                    LeaseType::PathWrite
                } else {
                    LeaseType::ProviderSlot
                }
            })?;
        let lease = Lease {
            id: format!("lease-{}-{item_id}-{acquired_at_ms}", self.run_id),
            lease_type,
            holder: holder.to_string(),
            acquired_at_ms,
            last_heartbeat_at_ms: None,
            ttl_ms: DEFAULT_WORKER_LEASE_TTL_MS,
        };
        let item = self
            .items
            .get_mut(item_id)
            .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.to_string()))?;
        item.lease = Some(lease.clone());
        Ok(lease)
    }
}

fn path_scope_is_explicit(scope: &opensks_contracts::PathScope) -> bool {
    scope.allow_external_write || !scope.workspace_relative_roots.is_empty()
}

fn path_scope_conflicts_any(
    candidate: &opensks_contracts::PathScope,
    admitted: &[opensks_contracts::PathScope],
) -> bool {
    admitted
        .iter()
        .any(|existing| path_scopes_conflict(candidate, existing))
}

fn path_scopes_conflict(
    left: &opensks_contracts::PathScope,
    right: &opensks_contracts::PathScope,
) -> bool {
    if !path_scope_is_explicit(left) || !path_scope_is_explicit(right) {
        return false;
    }
    if left.allow_external_write || right.allow_external_write {
        return true;
    }
    left.workspace_relative_roots.iter().any(|left_root| {
        right
            .workspace_relative_roots
            .iter()
            .any(|right_root| path_roots_overlap(left_root, right_root))
    })
}

fn path_roots_overlap(left: &str, right: &str) -> bool {
    let Some(left) = normalize_path_scope_root(left) else {
        return false;
    };
    let Some(right) = normalize_path_scope_root(right) else {
        return false;
    };
    left == right
        || right
            .strip_prefix(left.as_str())
            .is_some_and(|tail| tail.starts_with('/'))
        || left
            .strip_prefix(right.as_str())
            .is_some_and(|tail| tail.starts_with('/'))
}

fn normalize_path_scope_root(root: &str) -> Option<String> {
    let normalized = root
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect::<Vec<_>>()
        .join("/");
    if normalized.is_empty() || normalized.split('/').any(|part| part == "..") {
        None
    } else {
        Some(normalized)
    }
}

fn append_path_scope_payload(
    payload: &mut Map<String, Value>,
    evidence_refs: &mut Vec<String>,
    scope: &opensks_contracts::PathScope,
) -> Result<(), SchedulerError> {
    if !path_scope_is_explicit(scope) {
        return Ok(());
    }
    payload.insert("path_scope".to_string(), serde_json::to_value(scope)?);
    if !evidence_refs
        .iter()
        .any(|evidence_ref| evidence_ref == "scheduler:path-scope-bound")
    {
        evidence_refs.push("scheduler:path-scope-bound".to_string());
    }
    if scope.allow_external_write
        && !evidence_refs
            .iter()
            .any(|evidence_ref| evidence_ref == "scheduler:path-scope-external-write")
    {
        evidence_refs.push("scheduler:path-scope-external-write".to_string());
    }
    Ok(())
}

fn lease_last_seen_ms(lease: &Lease) -> u64 {
    lease.last_heartbeat_at_ms.unwrap_or(lease.acquired_at_ms)
}

fn lease_lifecycle_payload(
    lease: &Lease,
    item_id: &str,
    from: WorkState,
    to: WorkState,
) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "work_item_id".to_string(),
        Value::String(item_id.to_string()),
    );
    payload.insert("from".to_string(), Value::String(format!("{from:?}")));
    payload.insert("to".to_string(), Value::String(format!("{to:?}")));
    payload.insert("lease_id".to_string(), Value::String(lease.id.clone()));
    payload.insert(
        "lease_holder".to_string(),
        Value::String(lease.holder.clone()),
    );
    payload.insert(
        "lease_type".to_string(),
        serde_json::to_value(&lease.lease_type).unwrap_or(Value::Null),
    );
    payload.insert(
        "acquired_at_ms".to_string(),
        Value::Number(lease.acquired_at_ms.into()),
    );
    if let Some(last_heartbeat_at_ms) = lease.last_heartbeat_at_ms {
        payload.insert(
            "last_heartbeat_at_ms".to_string(),
            Value::Number(last_heartbeat_at_ms.into()),
        );
    }
    payload.insert("ttl_ms".to_string(), Value::Number(lease.ttl_ms.into()));
    payload
}

fn worker_payload(worker_id: &str, message: Option<&str>, ok: Option<bool>) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "worker_id".to_string(),
        Value::String(worker_id.to_string()),
    );
    if let Some(message) = message {
        payload.insert(
            "worker_message".to_string(),
            Value::String(message.to_string()),
        );
    }
    if let Some(ok) = ok {
        payload.insert("worker_ok".to_string(), Value::Bool(ok));
    }
    payload
}

fn worker_batch_payload(
    worker_id: &str,
    message: Option<&str>,
    ok: Option<bool>,
    batch_id: &str,
    batch_size: usize,
    lane_index: usize,
    worker_context_pack_ref: Option<&str>,
) -> Map<String, Value> {
    let mut payload = worker_payload(worker_id, message, ok);
    payload.insert("batch_id".to_string(), Value::String(batch_id.to_string()));
    payload.insert(
        "batch_size".to_string(),
        Value::Number((batch_size as u64).into()),
    );
    payload.insert(
        "batch_lane_index".to_string(),
        Value::Number((lane_index as u64).into()),
    );
    if let Some(worker_context_pack_ref) = worker_context_pack_ref {
        payload.insert(
            "worker_context_pack_ref".to_string(),
            Value::String(worker_context_pack_ref.to_string()),
        );
    }
    payload
}

fn scoped_worker_context_pack_ref(
    context_pack_ref: &str,
    item_id: &str,
    batch_id: &str,
    batch_size: usize,
    lane_index: usize,
) -> String {
    if let Some(worker_artifact_ref) = worker_context_artifact_ref(context_pack_ref, item_id) {
        return format!(
            "{}#work_item_id={}&batch_id={}&batch_size={}&batch_lane_index={}",
            worker_artifact_ref,
            context_ref_fragment_component(item_id),
            context_ref_fragment_component(batch_id),
            batch_size,
            lane_index
        );
    }
    let separator = if context_pack_ref.contains('#') {
        "&"
    } else {
        "#"
    };
    format!(
        "{}{}work_item_id={}&batch_id={}&batch_size={}&batch_lane_index={}",
        context_pack_ref,
        separator,
        context_ref_fragment_component(item_id),
        context_ref_fragment_component(batch_id),
        batch_size,
        lane_index
    )
}

fn worker_context_artifact_ref(context_pack_ref: &str, item_id: &str) -> Option<String> {
    let base_ref = context_pack_ref
        .split('#')
        .next()
        .unwrap_or(context_pack_ref);
    let relative = base_ref.strip_prefix("artifact://")?;
    let file_name = relative.rsplit('/').next()?;
    let stem = file_name.strip_suffix(".json")?;
    let directory_len = relative.len().saturating_sub(file_name.len());
    let directory = &relative[..directory_len];
    Some(format!(
        "artifact://{}{}--worker-{}.json",
        directory,
        stem,
        context_ref_fragment_component(item_id)
    ))
}

fn context_ref_fragment_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect()
}

fn provider_selector_from_model_id(model_id: &str) -> Option<String> {
    let (provider, _model) = model_id.split_once('/')?;
    let provider = provider.trim();
    if provider.is_empty() {
        None
    } else {
        Some(provider.to_string())
    }
}

pub fn make_work_item(
    run_id: &str,
    id: impl Into<String>,
    dependencies: Vec<String>,
) -> SchedulerWorkItem {
    let id = id.into();
    SchedulerWorkItem {
        schema: SCHEDULER_WORK_ITEM_SCHEMA.to_string(),
        id: id.clone(),
        run_id: run_id.to_string(),
        node_id: format!("node-{id}"),
        parent_id: None,
        kind: WorkKind::ModelInference,
        priority: 0,
        state: WorkState::Ready,
        dependencies,
        capability_requirements: opensks_contracts::CapabilityRequirements::text(),
        provider_selector: None,
        model_selector: None,
        context_pack_ref: None,
        worker_context_pack_ref: None,
        shard_policy_id: None,
        shard_policy_selection_policy: None,
        shard_policy_required_source_count: None,
        shard_policy_required_verifier_count: None,
        path_scope: opensks_contracts::PathScope::default(),
        budget: WorkBudget::default(),
        retry: opensks_contracts::RetryPolicy::default(),
        lease: None,
        idempotency_key: format!("idem-{run_id}-{id}"),
        requirement_ids: Vec::new(),
        evidence_refs: Vec::new(),
    }
}

pub fn conversation_turn_root_work_item(
    input: &ConversationTurnSchedulerInput<'_>,
) -> SchedulerWorkItem {
    let item_id = conversation_turn_root_work_item_id(input.turn_id);
    let mut evidence_refs = vec![
        "conversation:turn-accepted".to_string(),
        "scheduler:turn-root-bootstrap".to_string(),
    ];
    if input.context_pack_ref.is_some() {
        evidence_refs.push("context:turn-context-pack".to_string());
    }
    if input.resource_limits.is_some() {
        evidence_refs.push("scheduler:provider-registry-concurrency".to_string());
    }
    extend_role_plan_evidence(&mut evidence_refs, input.role_plan.as_ref());
    SchedulerWorkItem {
        schema: SCHEDULER_WORK_ITEM_SCHEMA.to_string(),
        id: item_id.clone(),
        run_id: input.run_id.to_string(),
        node_id: "conversation-turn-root".to_string(),
        parent_id: None,
        kind: WorkKind::Planning,
        priority: 100,
        state: WorkState::Ready,
        dependencies: Vec::new(),
        capability_requirements: CapabilityRequirements::code(),
        provider_selector: input
            .settings
            .model
            .model_id
            .as_deref()
            .and_then(provider_selector_from_model_id),
        model_selector: input.settings.model.model_id.clone(),
        context_pack_ref: input.context_pack_ref.map(str::to_string),
        worker_context_pack_ref: None,
        shard_policy_id: None,
        shard_policy_selection_policy: None,
        shard_policy_required_source_count: None,
        shard_policy_required_verifier_count: None,
        path_scope: opensks_contracts::PathScope::default(),
        budget: WorkBudget {
            max_attempts: 1,
            timeout_ms: input.settings.timeout_ms,
            max_cost_usd: input.settings.cost_budget_usd,
        },
        retry: opensks_contracts::RetryPolicy::default(),
        lease: None,
        idempotency_key: format!(
            "conversation-turn:{}:{}:{}",
            input.conversation_id, input.turn_id, input.settings_digest
        ),
        requirement_ids: vec![input.turn_id.to_string()],
        evidence_refs,
    }
}

pub fn conversation_turn_role_work_items(
    input: &ConversationTurnSchedulerInput<'_>,
    root_work_item_id: &str,
) -> Vec<SchedulerWorkItem> {
    let Some(role_plan) = input.role_plan.as_ref() else {
        return Vec::new();
    };
    role_plan
        .assignments
        .iter()
        .enumerate()
        .filter(|(_index, assignment)| assignment.status.has_resolved_model())
        .map(|(index, assignment)| {
            let role_label = scheduler_role_label(&assignment.role);
            let item_id = format!("turn-role-{}-{index}-{role_label}", input.turn_id);
            let mut evidence_refs = vec![
                "provider:role-routing".to_string(),
                "scheduler:role-plan-work-item".to_string(),
                "scheduler:hyperparallel-subcontract-planned".to_string(),
            ];
            if assignment.reused_model {
                evidence_refs.push("provider:single-model-role-reuse".to_string());
            }
            SchedulerWorkItem {
                schema: SCHEDULER_WORK_ITEM_SCHEMA.to_string(),
                id: item_id.clone(),
                run_id: input.run_id.to_string(),
                node_id: format!("conversation-turn-role-{role_label}"),
                parent_id: Some(root_work_item_id.to_string()),
                kind: work_kind_for_scheduler_role(&assignment.role),
                priority: priority_for_scheduler_role(&assignment.role),
                state: WorkState::Ready,
                dependencies: vec![root_work_item_id.to_string()],
                capability_requirements: capability_requirements_for_scheduler_role(
                    &assignment.role,
                ),
                provider_selector: assignment.provider_id.clone(),
                model_selector: assignment.selected_model_id.clone(),
                context_pack_ref: input.context_pack_ref.map(str::to_string),
                worker_context_pack_ref: None,
                shard_policy_id: None,
                shard_policy_selection_policy: None,
                shard_policy_required_source_count: None,
                shard_policy_required_verifier_count: None,
                path_scope: opensks_contracts::PathScope::default(),
                budget: WorkBudget {
                    max_attempts: 1,
                    timeout_ms: input.settings.timeout_ms,
                    max_cost_usd: input.settings.cost_budget_usd,
                },
                retry: opensks_contracts::RetryPolicy::default(),
                lease: None,
                idempotency_key: format!(
                    "conversation-turn-role:{}:{}:{}:{role_label}:{}",
                    input.conversation_id,
                    input.turn_id,
                    input.settings_digest,
                    assignment
                        .selected_model_id
                        .as_deref()
                        .unwrap_or("unassigned")
                ),
                requirement_ids: vec![input.turn_id.to_string(), role_label.to_string()],
                evidence_refs,
            }
        })
        .collect()
}

pub fn conversation_turn_objective_work_items(
    input: &ConversationTurnSchedulerInput<'_>,
    root_work_item_id: &str,
) -> Vec<SchedulerWorkItem> {
    let Some(plan) = input.objective_plan.as_ref() else {
        return Vec::new();
    };
    let model_selector = input.settings.model.model_id.clone();
    let provider_selector = model_selector
        .as_deref()
        .and_then(provider_selector_from_model_id);
    plan.work_items
        .iter()
        .cloned()
        .map(|mut item| {
            item.run_id = input.run_id.to_string();
            item.parent_id = Some(root_work_item_id.to_string());
            item.state = WorkState::Ready;
            item.lease = None;
            if item.dependencies.is_empty() {
                item.dependencies.push(root_work_item_id.to_string());
            }
            if item.provider_selector.is_none() {
                item.provider_selector = provider_selector.clone();
            }
            if item.model_selector.is_none() {
                item.model_selector = model_selector.clone();
            }
            if item.context_pack_ref.is_none() {
                item.context_pack_ref = input.context_pack_ref.map(str::to_string);
            }
            if !item
                .requirement_ids
                .iter()
                .any(|requirement_id| requirement_id == input.turn_id)
            {
                item.requirement_ids.insert(0, input.turn_id.to_string());
            }
            push_unique(
                &mut item.evidence_refs,
                "scheduler:objective-plan-work-item".to_string(),
            );
            push_unique(
                &mut item.evidence_refs,
                "graph:objective-planner".to_string(),
            );
            for evidence_ref in &plan.evidence_refs {
                push_unique(&mut item.evidence_refs, evidence_ref.clone());
            }
            item.idempotency_key = format!(
                "conversation-turn-objective:{}:{}:{}:{}",
                input.conversation_id, input.turn_id, input.settings_digest, item.id
            );
            item
        })
        .collect()
}

pub fn conversation_turn_scheduler_config(settings: &ConversationTurnSettings) -> SchedulerConfig {
    conversation_turn_scheduler_config_with_limits(settings, None)
}

pub fn conversation_turn_scheduler_config_with_limits(
    settings: &ConversationTurnSettings,
    resource_limits: Option<ConversationTurnSchedulerResourceLimits>,
) -> SchedulerConfig {
    let requested = settings.max_parallelism.max(1);
    let verification = settings.verifier_count.max(1);
    let provider_max_workers = resource_limits
        .map(|limits| limits.provider_max_workers.max(1).min(requested))
        .unwrap_or(requested);
    let per_provider_max_workers = resource_limits
        .map(|limits| {
            limits
                .per_provider_max_workers
                .max(1)
                .min(provider_max_workers)
        })
        .unwrap_or(requested);
    let per_model_max_workers = resource_limits
        .map(|limits| {
            limits
                .per_model_max_workers
                .max(1)
                .min(per_provider_max_workers)
        })
        .unwrap_or(requested);
    SchedulerConfig {
        requested_workers: requested,
        project_max_workers: requested,
        provider_max_workers,
        per_provider_max_workers,
        per_model_max_workers,
        worktree_max_workers: requested,
        verification_max_workers: verification,
        visible_lane_cap: requested.min(6),
    }
}

pub fn bootstrap_conversation_turn_scheduler(
    store: &mut EventStore,
    input: ConversationTurnSchedulerInput<'_>,
) -> Result<ConversationTurnSchedulerBootstrap, SchedulerError> {
    let work_item = conversation_turn_root_work_item(&input);
    let objective_work_items = conversation_turn_objective_work_items(&input, &work_item.id);
    let role_work_items = conversation_turn_role_work_items(&input, &work_item.id);
    let mut work_items = vec![work_item.clone()];
    work_items.extend(objective_work_items.clone());
    work_items.extend(role_work_items.clone());
    let scheduler = DurableScheduler::new(
        input.run_id,
        work_items.clone(),
        conversation_turn_scheduler_config_with_limits(input.settings, input.resource_limits),
    );
    let mut evidence_refs = vec![
        "conversation:turn-accepted".to_string(),
        "scheduler:turn-root-bootstrap".to_string(),
    ];
    if input.context_pack_ref.is_some() {
        evidence_refs.push("context:turn-context-pack".to_string());
    }
    if input.resource_limits.is_some() {
        evidence_refs.push("scheduler:provider-registry-concurrency".to_string());
    }
    extend_role_plan_evidence(&mut evidence_refs, input.role_plan.as_ref());
    extend_objective_plan_evidence(&mut evidence_refs, input.objective_plan.as_ref());
    let snapshot = scheduler.snapshot(
        format!("conversation-turn-accept:{}", input.now_ms),
        evidence_refs.clone(),
    );
    let existing_events = store.replay(input.run_id)?;
    let existing_queued_ids: BTreeSet<String> = existing_events
        .iter()
        .filter(|event| event.kind == EventKind::WorkItemQueued)
        .filter_map(|event| {
            event
                .payload
                .get("work_item_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    let existing = existing_events
        .iter()
        .filter(|event| event.kind == EventKind::WorkItemQueued)
        .filter(|event| {
            event.payload.get("work_item_id").and_then(Value::as_str) == Some(work_item.id.as_str())
        })
        .max_by_key(|event| event.sequence);
    if let Some(event) = existing {
        let root_sequence = event.sequence;
        let mut latest_sequence = root_sequence;
        for (index, objective_item) in objective_work_items.iter().enumerate() {
            if existing_queued_ids.contains(&objective_item.id) {
                continue;
            }
            let event =
                append_objective_work_item_queued_event(store, &input, objective_item, index)?;
            latest_sequence = latest_sequence.max(event.sequence);
        }
        for (index, role_item) in role_work_items.iter().enumerate() {
            if existing_queued_ids.contains(&role_item.id) {
                continue;
            }
            let event = append_role_work_item_queued_event(store, &input, role_item, index)?;
            latest_sequence = latest_sequence.max(event.sequence);
        }
        store.write_snapshot(
            input.run_id,
            latest_sequence,
            serde_json::to_value(&snapshot)?,
        )?;
        return Ok(ConversationTurnSchedulerBootstrap {
            work_item,
            work_items,
            snapshot,
            queued_event_sequence: root_sequence,
            reused: true,
        });
    }

    let event = ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{}-{}-queued", input.run_id, work_item.id),
        run_id: input.run_id.to_string(),
        sequence: 0,
        occurred_at: format!("conversation-turn-accepted:{}", input.now_ms),
        actor: "opensks-scheduler".to_string(),
        causation_id: Some(input.turn_id.to_string()),
        correlation_id: Some(work_item.id.clone()),
        kind: EventKind::WorkItemQueued,
        payload: serde_json::json!({
            "source": "conversation.turn_start",
            "work_item_id": work_item.id.clone(),
            "from": "Draft",
            "to": "Ready",
            "run_id": input.run_id,
            "turn_id": input.turn_id,
            "project_id": input.project_id,
            "conversation_id": input.conversation_id,
            "settings_digest": input.settings_digest,
            "pipeline_id": input.settings.pipeline_id,
            "execution_mode": input.settings.execution_mode,
            "max_parallelism": input.settings.max_parallelism,
            "verifier_count": input.settings.verifier_count,
            "work_budget_kind": "model_context_units",
            "work_budget_amount": input.settings.token_budget,
            "cost_budget_usd": input.settings.cost_budget_usd,
            "timeout_ms": input.settings.timeout_ms,
            "provider_max_workers": scheduler.config.provider_max_workers,
            "per_provider_max_workers": scheduler.config.per_provider_max_workers,
            "per_model_max_workers": scheduler.config.per_model_max_workers,
            "resource_limit_source": if input.resource_limits.is_some() { "provider_registry" } else { "thread_settings_default" },
            "role_plan_source": if input.role_plan.is_some() { "provider_registry" } else { "none" },
            "role_plan": input.role_plan.as_ref(),
            "role_work_item_count": role_work_items.len(),
            "objective_plan_source": input.objective_plan.as_ref().map(|plan| plan.source.as_str()).unwrap_or("none"),
            "objective_graph_id": input.objective_plan.as_ref().map(|plan| plan.graph_id.as_str()),
            "objective_plan_hash": input.objective_plan.as_ref().map(|plan| plan.plan_hash.as_str()),
            "objective_graph_ref": input.objective_plan.as_ref().and_then(|plan| plan.graph_ref.as_deref()),
            "objective_compiled_plan_ref": input.objective_plan.as_ref().and_then(|plan| plan.compiled_plan_ref.as_deref()),
            "objective_receipt_ref": input.objective_plan.as_ref().and_then(|plan| plan.receipt_ref.as_deref()),
            "objective_work_item_count": objective_work_items.len(),
            "tool_policy_id": input.settings.tool_policy_id,
            "approval_policy_id": input.settings.approval_policy_id,
            "context_pack_ref": input.context_pack_ref,
            "work_item": work_item.clone(),
        }),
        sensitivity: Sensitivity::Internal,
        evidence_refs,
    };
    let root_event = store.append_event(event)?;
    let mut latest_sequence = root_event.sequence;
    for (index, objective_item) in objective_work_items.iter().enumerate() {
        let event = append_objective_work_item_queued_event(store, &input, objective_item, index)?;
        latest_sequence = latest_sequence.max(event.sequence);
    }
    for (index, role_item) in role_work_items.iter().enumerate() {
        let event = append_role_work_item_queued_event(store, &input, role_item, index)?;
        latest_sequence = latest_sequence.max(event.sequence);
    }
    store.write_snapshot(
        input.run_id,
        latest_sequence,
        serde_json::to_value(&snapshot)?,
    )?;
    Ok(ConversationTurnSchedulerBootstrap {
        work_item,
        work_items,
        snapshot,
        queued_event_sequence: root_event.sequence,
        reused: false,
    })
}

fn append_role_work_item_queued_event(
    store: &mut EventStore,
    input: &ConversationTurnSchedulerInput<'_>,
    work_item: &SchedulerWorkItem,
    index: usize,
) -> Result<ExecutionEventEnvelope, SchedulerError> {
    let role_label = work_item
        .requirement_ids
        .iter()
        .find(|id| id.as_str() != input.turn_id)
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let mut evidence_refs = work_item.evidence_refs.clone();
    evidence_refs.push("scheduler:role-work-item-queued".to_string());
    let event = ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{}-{}-queued", input.run_id, work_item.id),
        run_id: input.run_id.to_string(),
        sequence: 0,
        occurred_at: format!("conversation-turn-role-queued:{}:{index}", input.now_ms),
        actor: "opensks-scheduler".to_string(),
        causation_id: Some(input.turn_id.to_string()),
        correlation_id: Some(work_item.id.clone()),
        kind: EventKind::WorkItemQueued,
        payload: serde_json::json!({
            "source": "conversation.role_plan",
            "work_item_id": work_item.id.clone(),
            "parent_work_item_id": work_item.parent_id.clone(),
            "from": "Draft",
            "to": "Ready",
            "run_id": input.run_id,
            "turn_id": input.turn_id,
            "project_id": input.project_id,
            "conversation_id": input.conversation_id,
            "settings_digest": input.settings_digest,
            "role": role_label,
            "provider_id": work_item.provider_selector.clone(),
            "model_id": work_item.model_selector.clone(),
            "context_pack_ref": input.context_pack_ref,
            "work_item": work_item.clone(),
        }),
        sensitivity: Sensitivity::Internal,
        evidence_refs,
    };
    store.append_event(event).map_err(SchedulerError::from)
}

fn append_objective_work_item_queued_event(
    store: &mut EventStore,
    input: &ConversationTurnSchedulerInput<'_>,
    work_item: &SchedulerWorkItem,
    index: usize,
) -> Result<ExecutionEventEnvelope, SchedulerError> {
    let plan = input.objective_plan.as_ref();
    let mut evidence_refs = work_item.evidence_refs.clone();
    push_unique(
        &mut evidence_refs,
        "scheduler:objective-plan-work-item-queued".to_string(),
    );
    if let Some(plan) = plan {
        for evidence_ref in &plan.evidence_refs {
            push_unique(&mut evidence_refs, evidence_ref.clone());
        }
    }
    let event = ExecutionEventEnvelope {
        schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
        id: format!("evt-{}-{}-queued", input.run_id, work_item.id),
        run_id: input.run_id.to_string(),
        sequence: 0,
        occurred_at: format!(
            "conversation-turn-objective-queued:{}:{index}",
            input.now_ms
        ),
        actor: "opensks-scheduler".to_string(),
        causation_id: Some(input.turn_id.to_string()),
        correlation_id: Some(work_item.id.clone()),
        kind: EventKind::WorkItemQueued,
        payload: serde_json::json!({
            "source": "conversation.objective_plan",
            "work_item_id": work_item.id.clone(),
            "parent_work_item_id": work_item.parent_id.clone(),
            "from": "Draft",
            "to": "Ready",
            "run_id": input.run_id,
            "turn_id": input.turn_id,
            "project_id": input.project_id,
            "conversation_id": input.conversation_id,
            "settings_digest": input.settings_digest,
            "objective_plan_source": plan.map(|plan| plan.source.as_str()).unwrap_or("unknown"),
            "objective_graph_id": plan.map(|plan| plan.graph_id.as_str()),
            "objective_plan_hash": plan.map(|plan| plan.plan_hash.as_str()),
            "objective_graph_ref": plan.and_then(|plan| plan.graph_ref.as_deref()),
            "objective_compiled_plan_ref": plan.and_then(|plan| plan.compiled_plan_ref.as_deref()),
            "objective_receipt_ref": plan.and_then(|plan| plan.receipt_ref.as_deref()),
            "objective_work_item_index": index,
            "work_kind": work_item.kind.clone(),
            "context_pack_ref": input.context_pack_ref,
            "work_item": work_item.clone(),
        }),
        sensitivity: Sensitivity::Internal,
        evidence_refs,
    };
    store.append_event(event).map_err(SchedulerError::from)
}

fn extend_role_plan_evidence(
    evidence_refs: &mut Vec<String>,
    role_plan: Option<&ConversationTurnSchedulerRolePlan>,
) {
    if let Some(role_plan) = role_plan {
        evidence_refs.push("provider:role-routing".to_string());
        evidence_refs.push("provider:health-cost-concurrency-scoring".to_string());
        if role_plan.reused_model_count > 0 {
            evidence_refs.push("provider:single-model-role-reuse".to_string());
        }
    }
}

fn extend_objective_plan_evidence(
    evidence_refs: &mut Vec<String>,
    objective_plan: Option<&ConversationTurnObjectivePlan>,
) {
    if let Some(objective_plan) = objective_plan {
        push_unique(
            evidence_refs,
            "scheduler:objective-plan-turn-bootstrap".to_string(),
        );
        push_unique(evidence_refs, "graph:objective-planner".to_string());
        for evidence_ref in &objective_plan.evidence_refs {
            push_unique(evidence_refs, evidence_ref.clone());
        }
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn scheduler_role_label(role: &ModelRole) -> &'static str {
    match role {
        ModelRole::General => "general",
        ModelRole::Planning => "planning",
        ModelRole::Code => "code",
        ModelRole::Verification => "verification",
        ModelRole::Vision => "vision",
        ModelRole::Image => "image",
        ModelRole::Arbiter => "arbiter",
    }
}

fn work_kind_for_scheduler_role(role: &ModelRole) -> WorkKind {
    match role {
        ModelRole::Planning => WorkKind::Planning,
        ModelRole::Verification => WorkKind::Verification,
        ModelRole::Arbiter => WorkKind::Approval,
        ModelRole::Code | ModelRole::General | ModelRole::Vision | ModelRole::Image => {
            WorkKind::ModelInference
        }
    }
}

fn priority_for_scheduler_role(role: &ModelRole) -> i32 {
    match role {
        ModelRole::Planning => 80,
        ModelRole::Code => 70,
        ModelRole::Verification => 60,
        ModelRole::Arbiter => 50,
        ModelRole::Vision | ModelRole::Image | ModelRole::General => 40,
    }
}

fn capability_requirements_for_scheduler_role(role: &ModelRole) -> CapabilityRequirements {
    match role {
        ModelRole::Code | ModelRole::Verification => CapabilityRequirements::code(),
        ModelRole::Planning | ModelRole::Arbiter => CapabilityRequirements {
            text: true,
            structured_output: true,
            ..CapabilityRequirements::default()
        },
        ModelRole::Vision => CapabilityRequirements {
            vision_input: true,
            ..CapabilityRequirements::default()
        },
        ModelRole::Image => CapabilityRequirements {
            image_output: true,
            ..CapabilityRequirements::default()
        },
        ModelRole::General => CapabilityRequirements::text(),
    }
}

pub fn conversation_turn_root_work_item_id(turn_id: &str) -> String {
    format!("turn-root-{turn_id}")
}

pub fn recover_completed_ids(
    store: &EventStore,
    run_id: &str,
) -> Result<BTreeSet<String>, SchedulerError> {
    let events = store.replay(run_id)?;
    let mut completed = BTreeSet::new();
    for event in events {
        if event.kind == EventKind::WorkItemCompleted {
            if let Some(work_item_id) = event
                .payload
                .get("work_item_id")
                .and_then(serde_json::Value::as_str)
            {
                completed.insert(work_item_id.to_string());
            }
        }
    }
    Ok(completed)
}

/// Fold the latest known [`WorkState`] for each work item from a run's events.
///
/// Each scheduler transition event carries `work_item_id` and a `to` state in
/// its payload; the last `to` per item wins. This lets callers validate steer
/// targets and reconstruct partial run state purely from the durable log.
pub fn recover_work_item_states(
    store: &EventStore,
    run_id: &str,
) -> Result<BTreeMap<String, WorkState>, SchedulerError> {
    let events = store.replay(run_id)?;
    let mut states = BTreeMap::new();
    for event in events {
        let work_item_id = event
            .payload
            .get("work_item_id")
            .and_then(serde_json::Value::as_str);
        let to_state = event
            .payload
            .get("to")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_work_state);
        if let (Some(work_item_id), Some(to_state)) = (work_item_id, to_state) {
            states.insert(work_item_id.to_string(), to_state);
        }
    }
    Ok(states)
}

fn parse_work_state(label: &str) -> Option<WorkState> {
    match label {
        "Draft" => Some(WorkState::Draft),
        "Ready" => Some(WorkState::Ready),
        "Leased" => Some(WorkState::Leased),
        "Dispatched" => Some(WorkState::Dispatched),
        "Running" => Some(WorkState::Running),
        "ResultReceived" => Some(WorkState::ResultReceived),
        "Verifying" => Some(WorkState::Verifying),
        "AwaitingApproval" => Some(WorkState::AwaitingApproval),
        "Applying" => Some(WorkState::Applying),
        "Completed" => Some(WorkState::Completed),
        "RetryWait" => Some(WorkState::RetryWait),
        "Blocked" => Some(WorkState::Blocked),
        "Failed" => Some(WorkState::Failed),
        "Cancelled" => Some(WorkState::Cancelled),
        "Superseded" => Some(WorkState::Superseded),
        _ => None,
    }
}

fn is_valid_transition(from: &WorkState, to: &WorkState) -> bool {
    if from.is_terminal() {
        return false;
    }
    matches!(
        (from, to),
        (WorkState::Ready, WorkState::Leased)
            | (WorkState::Leased, WorkState::Dispatched)
            | (WorkState::Leased, WorkState::Running)
            | (WorkState::Dispatched, WorkState::Running)
            | (WorkState::Running, WorkState::ResultReceived)
            | (WorkState::Running, WorkState::Completed)
            | (WorkState::ResultReceived, WorkState::Verifying)
            | (WorkState::Verifying, WorkState::AwaitingApproval)
            | (WorkState::Verifying, WorkState::Applying)
            | (WorkState::Applying, WorkState::Completed)
            | (_, WorkState::RetryWait)
            | (WorkState::RetryWait, WorkState::Ready)
            | (_, WorkState::Blocked)
            | (_, WorkState::Failed)
            | (_, WorkState::Cancelled)
            | (_, WorkState::Superseded)
    )
}

fn event_kind_for_state(state: &WorkState) -> EventKind {
    match state {
        WorkState::Leased => EventKind::WorkItemLeased,
        WorkState::Dispatched
        | WorkState::Running
        | WorkState::ResultReceived
        | WorkState::Verifying
        | WorkState::Applying => EventKind::WorkItemRunning,
        WorkState::Completed => EventKind::WorkItemCompleted,
        WorkState::Failed => EventKind::VerificationFailed,
        _ => EventKind::WorkItemQueued,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_settings(max_parallelism: u32) -> ConversationTurnSettings {
        ConversationTurnSettings {
            model: opensks_contracts::ModelSelection {
                mode: opensks_contracts::ModelSelectionMode::Pinned,
                model_id: Some("provider-1/code-model".to_string()),
                fallback_model_ids: Vec::new(),
            },
            reasoning_effort: opensks_contracts::ReasoningEffort::Standard,
            execution_mode: opensks_contracts::ExecutionMode::Worktree,
            pipeline_id: "parallel-build".to_string(),
            graph_revision: None,
            max_parallelism,
            verifier_count: 2,
            tool_policy_id: "project-default".to_string(),
            approval_policy_id: "safe-interactive".to_string(),
            token_budget: Some(64_000),
            cost_budget_usd: Some(1.25),
            timeout_ms: Some(120_000),
            image_model_id: None,
        }
    }

    #[test]
    fn conversation_turn_bootstrap_records_scheduler_root_item_idempotently() {
        let mut store = EventStore::open_memory().expect("event store");
        let settings = turn_settings(4);
        let role_plan = ConversationTurnSchedulerRolePlan {
            reason_code: "role_allocation_resolved_with_model_reuse".to_string(),
            assignments: vec![
                ConversationTurnSchedulerRoleAssignment {
                    role: ModelRole::Code,
                    status: RoutingStatus::Resolved,
                    selected_model_id: Some("provider-1/code-model".to_string()),
                    provider_id: Some("provider-1".to_string()),
                    reason_code: "single_enabled_compatible_model".to_string(),
                    reused_model: false,
                },
                ConversationTurnSchedulerRoleAssignment {
                    role: ModelRole::Verification,
                    status: RoutingStatus::Resolved,
                    selected_model_id: Some("provider-1/code-model".to_string()),
                    provider_id: Some("provider-1".to_string()),
                    reason_code: "single_enabled_compatible_model".to_string(),
                    reused_model: true,
                },
            ],
            distinct_model_count: 1,
            reused_model_count: 1,
            blocked_role_count: 0,
        };
        let input = ConversationTurnSchedulerInput {
            run_id: "run-turn-1",
            turn_id: "turn-1",
            project_id: "project-1",
            conversation_id: "conversation-1",
            settings: &settings,
            settings_digest: "sha256:v1:test",
            context_pack_ref: Some(
                "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1.json",
            ),
            resource_limits: None,
            role_plan: Some(role_plan.clone()),
            objective_plan: None,
            now_ms: 42_000,
        };

        let first = bootstrap_conversation_turn_scheduler(&mut store, input).expect("bootstrap");
        assert!(!first.reused);
        assert_eq!(first.queued_event_sequence, 1);
        assert_eq!(first.work_item.id, "turn-root-turn-1");
        assert_eq!(first.work_item.kind, WorkKind::Planning);
        assert_eq!(first.work_item.state, WorkState::Ready);
        assert_eq!(
            first.work_item.model_selector.as_deref(),
            Some("provider-1/code-model")
        );
        assert_eq!(
            first.work_item.provider_selector.as_deref(),
            Some("provider-1")
        );
        assert_eq!(first.work_item.budget.timeout_ms, Some(120_000));
        assert_eq!(first.work_item.budget.max_cost_usd, Some(1.25));
        assert_eq!(
            first.work_item.context_pack_ref.as_deref(),
            Some("artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1.json")
        );
        assert!(
            first
                .work_item
                .evidence_refs
                .iter()
                .any(|reference| reference == "context:turn-context-pack")
        );
        assert!(
            first
                .work_item
                .evidence_refs
                .contains(&"provider:role-routing".to_string())
        );
        assert!(
            first
                .work_item
                .evidence_refs
                .contains(&"provider:single-model-role-reuse".to_string())
        );
        assert_eq!(first.work_items.len(), 3);
        assert_eq!(first.snapshot.work_items.len(), 3);
        let code_role_item = first
            .work_items
            .iter()
            .find(|item| item.id == "turn-role-turn-1-0-code")
            .expect("code role work item");
        assert_eq!(
            code_role_item.parent_id.as_deref(),
            Some("turn-root-turn-1")
        );
        assert_eq!(
            code_role_item.dependencies,
            vec!["turn-root-turn-1".to_string()]
        );
        assert_eq!(code_role_item.kind, WorkKind::ModelInference);
        assert_eq!(
            code_role_item.provider_selector.as_deref(),
            Some("provider-1")
        );
        assert_eq!(
            code_role_item.model_selector.as_deref(),
            Some("provider-1/code-model")
        );
        assert!(
            code_role_item
                .evidence_refs
                .contains(&"scheduler:role-plan-work-item".to_string())
        );
        let verifier_role_item = first
            .work_items
            .iter()
            .find(|item| item.id == "turn-role-turn-1-1-verification")
            .expect("verification role work item");
        assert_eq!(verifier_role_item.kind, WorkKind::Verification);
        assert!(
            verifier_role_item
                .evidence_refs
                .contains(&"provider:single-model-role-reuse".to_string())
        );
        assert_eq!(first.snapshot.decision.requested, 4);
        assert_eq!(first.snapshot.decision.admitted, 4);
        assert_eq!(first.snapshot.decision.visible_lanes, 4);
        assert_eq!(first.snapshot.decision.limits["verification"], 2);

        let events = store.replay("run-turn-1").expect("replay");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, EventKind::WorkItemQueued);
        assert_eq!(events[0].payload["work_item_id"], "turn-root-turn-1");
        assert_eq!(events[0].payload["to"], "Ready");
        assert_eq!(events[0].payload["max_parallelism"], 4);
        assert_eq!(events[0].payload["work_budget_kind"], "model_context_units");
        assert_eq!(events[0].payload["work_budget_amount"], 64_000);
        assert_eq!(events[0].payload["cost_budget_usd"], 1.25);
        assert_eq!(events[0].payload["timeout_ms"], 120_000);
        assert_eq!(
            events[0].payload["context_pack_ref"],
            "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1.json"
        );
        assert_eq!(
            events[0].payload["work_item"]["context_pack_ref"],
            "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1.json"
        );
        assert_eq!(events[0].payload["work_item"]["kind"], "planning");
        assert_eq!(
            events[0].payload["work_item"]["provider_selector"],
            "provider-1"
        );
        assert_eq!(events[0].payload["role_plan_source"], "provider_registry");
        assert_eq!(
            events[0].payload["role_plan"]["reason_code"],
            "role_allocation_resolved_with_model_reuse"
        );
        assert_eq!(events[0].payload["role_plan"]["distinct_model_count"], 1);
        assert_eq!(events[0].payload["role_plan"]["reused_model_count"], 1);
        assert_eq!(
            events[0].payload["role_plan"]["assignments"][0]["selected_model_id"],
            "provider-1/code-model"
        );
        assert!(
            events[0]
                .evidence_refs
                .contains(&"provider:role-routing".to_string())
        );
        assert_eq!(events[0].payload["role_work_item_count"], 2);
        assert_eq!(events[1].payload["source"], "conversation.role_plan");
        assert_eq!(events[1].payload["parent_work_item_id"], "turn-root-turn-1");
        assert_eq!(events[1].payload["role"], "code");
        assert_eq!(events[1].payload["model_id"], "provider-1/code-model");
        assert_eq!(
            events[1].payload["work_item"]["dependencies"][0],
            "turn-root-turn-1"
        );
        assert!(
            events[1]
                .evidence_refs
                .contains(&"scheduler:role-work-item-queued".to_string())
        );
        assert_eq!(events[2].payload["role"], "verification");
        assert!(
            events[2]
                .evidence_refs
                .contains(&"provider:single-model-role-reuse".to_string())
        );
        let recovered_states = recover_work_item_states(&store, "run-turn-1").expect("states");
        assert_eq!(recovered_states["turn-root-turn-1"], WorkState::Ready);
        assert_eq!(
            recovered_states["turn-role-turn-1-0-code"],
            WorkState::Ready
        );
        assert_eq!(
            recovered_states["turn-role-turn-1-1-verification"],
            WorkState::Ready
        );

        let replay = bootstrap_conversation_turn_scheduler(
            &mut store,
            ConversationTurnSchedulerInput {
                run_id: "run-turn-1",
                turn_id: "turn-1",
                project_id: "project-1",
                conversation_id: "conversation-1",
                settings: &settings,
                settings_digest: "sha256:v1:test",
                context_pack_ref: Some(
                    "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-1.json",
                ),
                resource_limits: None,
                role_plan: Some(role_plan),
                objective_plan: None,
                now_ms: 43_000,
            },
        )
        .expect("idempotent bootstrap");
        assert!(replay.reused);
        assert_eq!(replay.queued_event_sequence, 1);
        assert_eq!(replay.work_items.len(), 3);
        assert_eq!(store.replay("run-turn-1").expect("replay again").len(), 3);
    }

    #[test]
    fn conversation_turn_objective_plan_queues_compiled_work_items_idempotently() {
        let mut store = EventStore::open_memory().expect("event store");
        let mut settings = turn_settings(6);
        settings.pipeline_id = "objective-planner".to_string();
        settings.model.model_id = Some("provider-1/code-model".to_string());
        let mut plan_item = make_work_item("run-turn-objective", "work-template-goal", Vec::new());
        plan_item.node_id = "goal".to_string();
        plan_item.kind = WorkKind::Planning;
        plan_item.requirement_ids = vec!["req-objective-understood".to_string()];
        plan_item.evidence_refs = vec!["graph:objective-planner".to_string()];
        let objective_plan = ConversationTurnObjectivePlan {
            graph_id: "objective-plan-test".to_string(),
            plan_hash: "sha256:objective-test".to_string(),
            source: "objective_planner".to_string(),
            graph_ref: Some(
                "artifact://.opensks/runtime/objective-plans/run/graph.json".to_string(),
            ),
            compiled_plan_ref: Some(
                "artifact://.opensks/runtime/objective-plans/run/compiled-plan.json".to_string(),
            ),
            receipt_ref: Some(
                "artifact://.opensks/runtime/objective-plans/run/receipt.json".to_string(),
            ),
            work_items: vec![plan_item],
            evidence_refs: vec!["daemon:conversation-turn-objective-planner-bootstrap".to_string()],
        };

        let first = bootstrap_conversation_turn_scheduler(
            &mut store,
            ConversationTurnSchedulerInput {
                run_id: "run-turn-objective",
                turn_id: "turn-objective",
                project_id: "project-1",
                conversation_id: "conversation-1",
                settings: &settings,
                settings_digest: "sha256:v1:objective",
                context_pack_ref: Some("artifact://turn-context-objective.json"),
                resource_limits: None,
                role_plan: None,
                objective_plan: Some(objective_plan.clone()),
                now_ms: 44_000,
            },
        )
        .expect("bootstrap objective plan");

        assert!(!first.reused);
        assert_eq!(first.work_items.len(), 2);
        let objective_item = first
            .work_items
            .iter()
            .find(|item| item.id == "work-template-goal")
            .expect("objective work item");
        assert_eq!(
            objective_item.parent_id.as_deref(),
            Some("turn-root-turn-objective")
        );
        assert_eq!(
            objective_item.dependencies,
            vec!["turn-root-turn-objective".to_string()]
        );
        assert_eq!(
            objective_item.model_selector.as_deref(),
            Some("provider-1/code-model")
        );
        assert_eq!(
            objective_item.provider_selector.as_deref(),
            Some("provider-1")
        );
        assert!(
            objective_item
                .requirement_ids
                .iter()
                .any(|id| id == "turn-objective")
        );
        assert!(
            objective_item
                .evidence_refs
                .iter()
                .any(|evidence| evidence == "scheduler:objective-plan-work-item")
        );

        let events = store.replay("run-turn-objective").expect("replay");
        assert_eq!(events.len(), 2);
        let root_event = events
            .iter()
            .find(|event| event.payload["source"] == "conversation.turn_start")
            .expect("root event");
        assert_eq!(
            root_event.payload["objective_plan_source"],
            "objective_planner"
        );
        assert_eq!(
            root_event.payload["objective_graph_id"],
            "objective-plan-test"
        );
        assert_eq!(
            root_event.payload["objective_receipt_ref"],
            "artifact://.opensks/runtime/objective-plans/run/receipt.json"
        );
        assert_eq!(root_event.payload["objective_work_item_count"], 1);
        let objective_event = events
            .iter()
            .find(|event| event.payload["source"] == "conversation.objective_plan")
            .expect("objective event");
        assert_eq!(
            objective_event.payload["parent_work_item_id"],
            "turn-root-turn-objective"
        );
        assert_eq!(
            objective_event.payload["objective_graph_id"],
            "objective-plan-test"
        );
        assert_eq!(
            objective_event.payload["objective_compiled_plan_ref"],
            "artifact://.opensks/runtime/objective-plans/run/compiled-plan.json"
        );
        assert!(
            objective_event
                .evidence_refs
                .iter()
                .any(|evidence| { evidence == "scheduler:objective-plan-work-item-queued" })
        );

        let replay = bootstrap_conversation_turn_scheduler(
            &mut store,
            ConversationTurnSchedulerInput {
                run_id: "run-turn-objective",
                turn_id: "turn-objective",
                project_id: "project-1",
                conversation_id: "conversation-1",
                settings: &settings,
                settings_digest: "sha256:v1:objective",
                context_pack_ref: Some("artifact://turn-context-objective.json"),
                resource_limits: None,
                role_plan: None,
                objective_plan: Some(objective_plan),
                now_ms: 45_000,
            },
        )
        .expect("idempotent objective bootstrap");
        assert!(replay.reused);
        assert_eq!(
            store
                .replay("run-turn-objective")
                .expect("replay again")
                .len(),
            2
        );
    }

    #[test]
    fn scheduler_simulates_ten_thousand_items_without_loss() {
        let run_id = "run-10k";
        let items: Vec<_> = (0..10_000)
            .map(|index| make_work_item(run_id, format!("wi-{index:05}"), Vec::new()))
            .collect();
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 100,
                project_max_workers: 32,
                provider_max_workers: 28,
                per_provider_max_workers: 28,
                per_model_max_workers: 28,
                worktree_max_workers: 30,
                verification_max_workers: 20,
                visible_lane_cap: 12,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let snapshot = scheduler
            .simulate_until_idle(&mut store)
            .expect("simulate scheduler");
        assert_eq!(snapshot.work_items.len(), 10_000);
        assert!(
            snapshot
                .work_items
                .iter()
                .all(|item| item.state == WorkState::Completed)
        );
        assert_eq!(snapshot.decision.admitted, 28);
        assert_eq!(snapshot.max_concurrent_workers, 28);
    }

    #[test]
    fn dependencies_release_after_completed_event() {
        let run_id = "run-deps";
        let items = vec![
            make_work_item(run_id, "first", Vec::new()),
            make_work_item(run_id, "second", vec!["first".to_string()]),
        ];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        assert_eq!(scheduler.ready_items(), vec!["first"]);
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .transition(&mut store, "first", WorkState::Leased, Vec::new())
            .expect("lease first");
        scheduler
            .transition(&mut store, "first", WorkState::Running, Vec::new())
            .expect("run first");
        scheduler
            .transition(
                &mut store,
                "first",
                WorkState::Completed,
                vec!["proof".to_string()],
            )
            .expect("complete first");
        assert_eq!(scheduler.ready_items(), vec!["second"]);
        let completed = recover_completed_ids(&store, run_id).expect("recover");
        assert!(completed.contains("first"));
    }

    #[test]
    fn state_mutation_is_blocked_when_event_append_fails() {
        let run_id = "run-fail";
        let mut item = make_work_item(run_id, "item", Vec::new());
        item.state = WorkState::Completed;
        let mut scheduler = DurableScheduler::new(run_id, vec![item], SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        let error = scheduler
            .transition(&mut store, "item", WorkState::Running, Vec::new())
            .expect_err("terminal transition must fail");
        assert!(matches!(error, SchedulerError::InvalidTransition { .. }));
        assert_eq!(scheduler.work_items()[0].state, WorkState::Completed);
    }

    #[test]
    fn worker_dispatch_completes_items_with_leases_and_report() {
        let run_id = "run-worker-dispatch";
        let items = vec![
            make_work_item(run_id, "first", Vec::new()),
            make_work_item(run_id, "second", Vec::new()),
        ];
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 4,
                project_max_workers: 4,
                provider_max_workers: 4,
                per_provider_max_workers: 4,
                per_model_max_workers: 4,
                worktree_max_workers: 4,
                verification_max_workers: 2,
                visible_lane_cap: 2,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = DeterministicWorker::new("test-worker");
        let (snapshot, report) = scheduler
            .dispatch_until_idle(&mut store, &mut worker)
            .expect("dispatch scheduler");
        assert_eq!(report.attempted, 2);
        assert_eq!(report.completed, 2);
        assert_eq!(report.failed, 0);
        assert_eq!(report.worker_ids, vec!["test-worker"]);
        assert_eq!(report.max_parallel_batch_size, 2);
        assert_eq!(report.parallel_batches, 1);
        assert_eq!(snapshot.max_concurrent_workers, 2);
        assert!(snapshot.work_items.iter().all(|item| {
            item.state == WorkState::Completed
                && item
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.holder == "test-worker")
                && item
                    .evidence_refs
                    .contains(&"scheduler:worker-dispatch".to_string())
        }));
        let events = store.replay(run_id).expect("replay dispatch");
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::WorkItemLeased
                    && event.payload["lease_holder"] == "test-worker")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::WorkItemRunning
                    && event.payload["to"] == "Dispatched")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::WorkItemCompleted)
        );
        assert!(events.iter().any(|event| {
            event.kind == EventKind::WorkItemCompleted
                && event.payload["fencing_token"] == event.payload["lease_id"]
                && event.payload["fencing_holder"] == "test-worker"
        }));
    }

    #[derive(Debug, Default, Clone)]
    struct LeaseCapturingWorker {
        seen: std::sync::Arc<std::sync::Mutex<Vec<SchedulerWorkItem>>>,
    }

    impl WorkerDriver for LeaseCapturingWorker {
        fn acquire_holder(&mut self, item: &SchedulerWorkItem) -> String {
            assert!(
                item.lease.is_none(),
                "holder selection runs before lease acquisition"
            );
            format!("lease-lane-{}", item.id)
        }

        fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome {
            self.seen
                .lock()
                .expect("lease capture lock")
                .push(item.clone());
            let lease = item.lease.as_ref().expect("execute receives active lease");
            WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: lease.holder.clone(),
                ok: true,
                message: format!("lease-bound worker completed {}", item.id),
                evidence_refs: vec!["scheduler:lease-visible-to-worker".to_string()],
            }
        }
    }

    #[test]
    fn worker_execute_receives_scheduler_lease_for_downstream_patch_fencing() {
        let run_id = "run-worker-patch-fence";
        let items = vec![make_work_item(run_id, "first", Vec::new())];
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 1,
                project_max_workers: 1,
                provider_max_workers: 1,
                per_provider_max_workers: 1,
                per_model_max_workers: 1,
                worktree_max_workers: 1,
                verification_max_workers: 1,
                visible_lane_cap: 1,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = LeaseCapturingWorker::default();
        let report = scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect("dispatch one worker");

        assert_eq!(report.completed, 1);
        let seen = worker.seen.lock().expect("lease capture lock").clone();
        assert_eq!(seen.len(), 1);
        let lease = seen[0].lease.as_ref().expect("active lease in worker item");
        assert_eq!(lease.holder, "lease-lane-first");
        assert!(lease.id.starts_with("lease-run-worker-patch-fence-first-"));
        assert!(
            report.outcomes[0]
                .evidence_refs
                .contains(&"scheduler:lease-visible-to-worker".to_string())
        );
    }

    #[test]
    fn path_scoped_work_item_records_path_scope_on_scheduler_lease_events() {
        let run_id = "run-path-scope-evidence";
        let mut item = make_work_item(run_id, "first", Vec::new());
        item.path_scope = opensks_contracts::PathScope {
            workspace_relative_roots: vec!["crates/opensks-scheduler".to_string()],
            allow_external_write: false,
        };
        let mut scheduler = DurableScheduler::new(
            run_id,
            vec![item],
            SchedulerConfig {
                requested_workers: 1,
                project_max_workers: 1,
                provider_max_workers: 1,
                per_provider_max_workers: 1,
                per_model_max_workers: 1,
                worktree_max_workers: 1,
                verification_max_workers: 1,
                visible_lane_cap: 1,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = LeaseCapturingWorker::default();

        scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect("dispatch path scoped worker");

        let seen = worker.seen.lock().expect("lease capture lock").clone();
        assert_eq!(
            seen[0].path_scope.workspace_relative_roots,
            vec!["crates/opensks-scheduler".to_string()]
        );
        let lease = seen[0].lease.as_ref().expect("path scope lease");
        assert_eq!(lease.lease_type, LeaseType::PathWrite);
        let events = store.replay(run_id).expect("replay path scope events");
        let leased = events
            .iter()
            .find(|event| event.kind == EventKind::WorkItemLeased)
            .expect("leased event");
        assert_eq!(
            leased.payload["path_scope"]["workspace_relative_roots"][0],
            "crates/opensks-scheduler"
        );
        assert_eq!(leased.payload["path_scope"]["allow_external_write"], false);
        assert_eq!(leased.payload["lease_type"], "path_write");
        assert!(
            leased
                .evidence_refs
                .contains(&"scheduler:path-scope-bound".to_string())
        );
        assert!(
            events.iter().any(|event| {
                event.kind == EventKind::WorkItemRunning
                    && event
                        .evidence_refs
                        .contains(&"scheduler:path-scope-bound".to_string())
                    && event.payload["path_scope"]["workspace_relative_roots"][0]
                        == "crates/opensks-scheduler"
            }),
            "path scope should stay visible across fenced dispatch/running events"
        );
    }

    #[test]
    fn worker_dispatch_serializes_overlapping_path_scopes_in_batch_admission() {
        let run_id = "run-path-scope-admission";
        let mut scheduler_item = make_work_item(run_id, "scheduler", Vec::new());
        scheduler_item.path_scope = opensks_contracts::PathScope {
            workspace_relative_roots: vec!["crates/opensks-scheduler".to_string()],
            allow_external_write: false,
        };
        let mut nested_item = make_work_item(run_id, "scheduler-src", Vec::new());
        nested_item.path_scope = opensks_contracts::PathScope {
            workspace_relative_roots: vec!["crates/opensks-scheduler/src".to_string()],
            allow_external_write: false,
        };
        let mut cli_item = make_work_item(run_id, "cli", Vec::new());
        cli_item.path_scope = opensks_contracts::PathScope {
            workspace_relative_roots: vec!["crates/opensks-cli".to_string()],
            allow_external_write: false,
        };
        let mut scheduler = DurableScheduler::new(
            run_id,
            vec![scheduler_item, nested_item, cli_item],
            SchedulerConfig {
                requested_workers: 3,
                project_max_workers: 3,
                provider_max_workers: 3,
                per_provider_max_workers: 3,
                per_model_max_workers: 3,
                worktree_max_workers: 3,
                verification_max_workers: 1,
                visible_lane_cap: 3,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = ParallelProbeWorker::new();

        let first_report = scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect("dispatch first path-scoped batch");

        assert_eq!(first_report.completed, 2);
        let first_completed = first_report
            .outcomes
            .iter()
            .map(|outcome| outcome.work_item_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(first_completed, vec!["cli", "scheduler"]);
        let states = scheduler
            .work_items()
            .into_iter()
            .map(|item| (item.id, item.state))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(states["scheduler"], WorkState::Completed);
        assert_eq!(states["cli"], WorkState::Completed);
        assert_eq!(states["scheduler-src"], WorkState::Ready);
        let first_events = store.replay(run_id).expect("replay first path batch");
        assert!(
            !first_events
                .iter()
                .any(|event| event.payload["work_item_id"] == "scheduler-src"),
            "overlapping nested path scope must wait for a later batch"
        );

        let second_report = scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect("dispatch deferred nested path scope");

        assert_eq!(second_report.completed, 1);
        assert_eq!(second_report.outcomes[0].work_item_id, "scheduler-src");
        let second_events = store.replay(run_id).expect("replay second path batch");
        assert!(second_events.iter().any(|event| {
            event.payload["work_item_id"] == "scheduler-src"
                && event.payload["lease_type"] == "path_write"
                && event
                    .evidence_refs
                    .contains(&"scheduler:path-scope-bound".to_string())
        }));
    }

    #[derive(Debug, Clone)]
    struct ParallelProbeWorker {
        active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        max_overlap: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ParallelProbeWorker {
        fn new() -> Self {
            Self {
                active: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                max_overlap: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn max_overlap(&self) -> usize {
            self.max_overlap.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn parallel_probe_outcome(item: SchedulerWorkItem) -> WorkerDispatchOutcome {
        let worker_id = item
            .lease
            .as_ref()
            .map(|lease| lease.holder.clone())
            .expect("parallel probe item has an active lease");
        WorkerDispatchOutcome {
            work_item_id: item.id.clone(),
            worker_id: worker_id.clone(),
            ok: true,
            message: format!("parallel probe completed {}", item.id),
            evidence_refs: vec![
                "scheduler:parallel-probe-worker".to_string(),
                format!("worker:{worker_id}:result"),
            ],
        }
    }

    fn selected_work_item(
        run_id: &str,
        id: &str,
        provider_selector: &str,
        model_selector: &str,
    ) -> SchedulerWorkItem {
        let mut item = make_work_item(run_id, id, Vec::new());
        item.provider_selector = Some(provider_selector.to_string());
        item.model_selector = Some(model_selector.to_string());
        item
    }

    impl WorkerDriver for ParallelProbeWorker {
        fn acquire_holder(&mut self, item: &SchedulerWorkItem) -> String {
            format!("lane-{}", item.id)
        }

        fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome {
            parallel_probe_outcome(item.clone())
        }

        fn execute_batch(&mut self, items: Vec<SchedulerWorkItem>) -> Vec<WorkerDispatchOutcome> {
            if items.is_empty() {
                return Vec::new();
            }
            let worker_count = items.len();
            let start = std::sync::Arc::new(std::sync::Barrier::new(worker_count));
            let all_active = std::sync::Arc::new(std::sync::Barrier::new(worker_count));
            let mut handles = Vec::with_capacity(worker_count);
            for item in items {
                let start = std::sync::Arc::clone(&start);
                let all_active = std::sync::Arc::clone(&all_active);
                let active = std::sync::Arc::clone(&self.active);
                let max_overlap = std::sync::Arc::clone(&self.max_overlap);
                handles.push(std::thread::spawn(move || {
                    start.wait();
                    let current = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    max_overlap.fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                    all_active.wait();
                    active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    parallel_probe_outcome(item)
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("parallel probe worker joined"))
                .collect()
        }
    }

    #[test]
    fn worker_dispatch_overlaps_admitted_workers_and_rejects_duplicate_outcomes() {
        let run_id = "run-parallel-dispatch";
        let items: Vec<_> = (0..8)
            .map(|index| make_work_item(run_id, format!("wi-{index}"), Vec::new()))
            .collect();
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 8,
                project_max_workers: 4,
                provider_max_workers: 4,
                per_provider_max_workers: 4,
                per_model_max_workers: 4,
                worktree_max_workers: 4,
                verification_max_workers: 2,
                visible_lane_cap: 4,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = ParallelProbeWorker::new();
        let (snapshot, report) = scheduler
            .dispatch_until_idle(&mut store, &mut worker)
            .expect("parallel dispatch scheduler");

        assert_eq!(report.decision.admitted, 4);
        assert_eq!(report.attempted, 8);
        assert_eq!(report.completed, 8);
        assert_eq!(report.failed, 0);
        assert_eq!(report.max_parallel_batch_size, 4);
        assert_eq!(report.parallel_batches, 2);
        assert_eq!(report.worker_ids.len(), 8);
        assert_eq!(snapshot.max_concurrent_workers, 4);
        assert_eq!(snapshot.overlap_ratio, 1.0);
        assert!(
            snapshot
                .evidence_refs
                .contains(&"scheduler:parallel-batch-dispatch".to_string())
        );
        assert!(worker.max_overlap() >= 4);

        let mut outcome_ids = std::collections::BTreeSet::new();
        for outcome in &report.outcomes {
            assert!(
                outcome_ids.insert(outcome.work_item_id.clone()),
                "duplicate worker outcome for {}",
                outcome.work_item_id
            );
        }
        assert_eq!(outcome_ids.len(), 8);

        let events = store.replay(run_id).expect("replay parallel dispatch");
        let first_result_index = events
            .iter()
            .position(|event| {
                event.kind == EventKind::WorkItemRunning && event.payload["to"] == "ResultReceived"
            })
            .expect("first result event");
        let running_before_first_result: Vec<_> = events
            .iter()
            .take(first_result_index)
            .filter(|event| {
                event.kind == EventKind::WorkItemRunning
                    && event.payload["to"] == "Running"
                    && event.payload["batch_size"] == 4
                    && event
                        .evidence_refs
                        .contains(&"scheduler:parallel-batch-dispatch".to_string())
            })
            .collect();
        assert_eq!(running_before_first_result.len(), 4);
        let first_batch_id = running_before_first_result[0].payload["batch_id"]
            .as_str()
            .expect("batch id");
        assert!(
            running_before_first_result
                .iter()
                .all(|event| { event.payload["batch_id"].as_str() == Some(first_batch_id) })
        );
    }

    #[test]
    fn worker_dispatch_respects_provider_and_model_semaphores() {
        let run_id = "run-provider-model-semaphore";
        let items = vec![
            selected_work_item(run_id, "a-model-a-1", "provider-a", "provider-a/model-a"),
            selected_work_item(run_id, "a-model-a-2", "provider-a", "provider-a/model-a"),
            selected_work_item(run_id, "a-model-b-1", "provider-a", "provider-a/model-b"),
            selected_work_item(run_id, "a-model-c-1", "provider-a", "provider-a/model-c"),
            selected_work_item(run_id, "b-model-d-1", "provider-b", "provider-b/model-d"),
        ];
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 4,
                project_max_workers: 4,
                provider_max_workers: 4,
                per_provider_max_workers: 2,
                per_model_max_workers: 1,
                worktree_max_workers: 4,
                verification_max_workers: 2,
                visible_lane_cap: 4,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = ParallelProbeWorker::new();
        let report = scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect("dispatch semaphore-limited batch");

        assert_eq!(report.decision.limits["per_provider"], 2);
        assert_eq!(report.decision.limits["per_model"], 1);
        assert_eq!(report.attempted, 3);
        assert_eq!(report.completed, 3);
        assert_eq!(report.max_parallel_batch_size, 3);
        let completed_ids = report
            .outcomes
            .iter()
            .map(|outcome| outcome.work_item_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            completed_ids,
            vec!["a-model-a-1", "a-model-b-1", "b-model-d-1"]
        );
        assert!(report.outcomes.iter().all(|outcome| {
            outcome
                .evidence_refs
                .contains(&"scheduler:provider-model-semaphore".to_string())
        }));

        let states = scheduler
            .work_items()
            .into_iter()
            .map(|item| (item.id, item.state))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(states["a-model-a-1"], WorkState::Completed);
        assert_eq!(states["a-model-b-1"], WorkState::Completed);
        assert_eq!(states["b-model-d-1"], WorkState::Completed);
        assert_eq!(states["a-model-a-2"], WorkState::Ready);
        assert_eq!(states["a-model-c-1"], WorkState::Ready);

        let events = store.replay(run_id).expect("replay semaphore dispatch");
        let running_events = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::WorkItemRunning && event.payload["to"] == "Running"
            })
            .collect::<Vec<_>>();
        assert_eq!(running_events.len(), 3);
        assert!(running_events.iter().all(|event| {
            event
                .evidence_refs
                .contains(&"scheduler:provider-model-semaphore".to_string())
        }));
        assert!(!events.iter().any(|event| {
            event.payload["work_item_id"] == "a-model-a-2"
                || event.payload["work_item_id"] == "a-model-c-1"
        }));
    }

    #[derive(Debug, Clone, Default)]
    struct ContextCapturingWorker {
        seen: std::sync::Arc<std::sync::Mutex<Vec<SchedulerWorkItem>>>,
    }

    impl ContextCapturingWorker {
        fn seen(&self) -> Vec<SchedulerWorkItem> {
            self.seen.lock().expect("context capture lock").clone()
        }
    }

    impl WorkerDriver for ContextCapturingWorker {
        fn acquire_holder(&mut self, item: &SchedulerWorkItem) -> String {
            assert!(
                item.worker_context_pack_ref.is_some(),
                "holder selection receives worker-scoped context"
            );
            format!("context-lane-{}", item.id)
        }

        fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome {
            self.seen
                .lock()
                .expect("context capture lock")
                .push(item.clone());
            parallel_probe_outcome(item.clone())
        }
    }

    #[test]
    fn parallel_dispatch_hands_each_worker_a_scoped_context_pack_ref() {
        let run_id = "run-worker-context";
        let root_context_pack_ref =
            "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-ctx.json";
        let items: Vec<_> = (0..4)
            .map(|index| {
                let mut item = make_work_item(run_id, format!("wi-{index}"), Vec::new());
                item.context_pack_ref = Some(root_context_pack_ref.to_string());
                item
            })
            .collect();
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 4,
                project_max_workers: 4,
                provider_max_workers: 4,
                per_provider_max_workers: 4,
                per_model_max_workers: 4,
                worktree_max_workers: 4,
                verification_max_workers: 2,
                visible_lane_cap: 4,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = ContextCapturingWorker::default();
        let (snapshot, report) = scheduler
            .dispatch_until_idle(&mut store, &mut worker)
            .expect("context-scoped dispatch");

        assert_eq!(report.completed, 4);
        assert_eq!(report.max_parallel_batch_size, 4);
        let captured = worker.seen();
        assert_eq!(captured.len(), 4);
        let mut scoped_refs = std::collections::BTreeSet::new();
        for item in &captured {
            assert_eq!(
                item.context_pack_ref.as_deref(),
                Some(root_context_pack_ref)
            );
            let scoped_ref = item
                .worker_context_pack_ref
                .as_deref()
                .expect("worker-scoped context ref");
            assert!(
                scoped_ref.starts_with(
                    "artifact://.opensks/wiki/context-packs/generated/turn-context-turn-ctx--worker-"
                ),
                "scoped ref points at a deterministic worker artifact"
            );
            assert!(
                scoped_ref.contains(".json#"),
                "scoped ref has an artifact path plus dispatch metadata"
            );
            assert!(
                scoped_ref.contains(&format!("work_item_id={}", item.id)),
                "scoped ref carries the work item id"
            );
            assert!(
                scoped_ref.contains("batch_size=4"),
                "scoped ref carries the batch shape"
            );
            assert!(
                scoped_refs.insert(scoped_ref.to_string()),
                "scoped ref is unique per worker"
            );
        }
        assert_eq!(scoped_refs.len(), 4);
        assert!(snapshot.work_items.iter().all(|item| {
            item.context_pack_ref.as_deref() == Some(root_context_pack_ref)
                && item
                    .worker_context_pack_ref
                    .as_deref()
                    .is_some_and(|reference| reference.contains("--worker-"))
                && item
                    .evidence_refs
                    .contains(&"context:worker-context-pack".to_string())
        }));

        let events = store.replay(run_id).expect("replay context dispatch");
        let running_events: Vec<_> = events
            .iter()
            .filter(|event| {
                event.kind == EventKind::WorkItemRunning && event.payload["to"] == "Running"
            })
            .collect();
        assert_eq!(running_events.len(), 4);
        for event in running_events {
            let work_item_id = event.payload["work_item_id"]
                .as_str()
                .expect("work item id");
            let scoped_ref = event.payload["worker_context_pack_ref"]
                .as_str()
                .expect("worker context ref payload");
            assert!(scoped_ref.contains(&format!("work_item_id={work_item_id}")));
            assert!(
                event
                    .evidence_refs
                    .contains(&"context:worker-context-pack".to_string())
            );
        }
    }

    struct DuplicateOutcomeWorker;

    impl WorkerDriver for DuplicateOutcomeWorker {
        fn acquire_holder(&mut self, item: &SchedulerWorkItem) -> String {
            format!("duplicate-lane-{}", item.id)
        }

        fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome {
            parallel_probe_outcome(item.clone())
        }

        fn execute_batch(&mut self, items: Vec<SchedulerWorkItem>) -> Vec<WorkerDispatchOutcome> {
            let first = items
                .first()
                .cloned()
                .expect("duplicate worker receives a batch");
            vec![
                parallel_probe_outcome(first.clone()),
                parallel_probe_outcome(first),
            ]
        }
    }

    #[test]
    fn worker_dispatch_rejects_duplicate_batch_outcomes_before_completion() {
        let run_id = "run-duplicate-outcome";
        let items = vec![
            make_work_item(run_id, "first", Vec::new()),
            make_work_item(run_id, "second", Vec::new()),
        ];
        let mut scheduler = DurableScheduler::new(
            run_id,
            items,
            SchedulerConfig {
                requested_workers: 2,
                project_max_workers: 2,
                provider_max_workers: 2,
                per_provider_max_workers: 2,
                per_model_max_workers: 2,
                worktree_max_workers: 2,
                verification_max_workers: 1,
                visible_lane_cap: 2,
            },
        );
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = DuplicateOutcomeWorker;
        let error = scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect_err("duplicate outcome rejected");
        assert!(matches!(
            error,
            SchedulerError::DuplicateWorkerOutcome(item_id) if item_id == "first"
        ));
        let events = store.replay(run_id).expect("replay duplicate outcome");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == EventKind::WorkItemRunning
                    && event.payload["to"] == "Running")
                .count(),
            2
        );
        assert!(
            !events
                .iter()
                .any(|event| event.kind == EventKind::WorkItemCompleted)
        );
    }

    struct FailingWorker;

    impl WorkerDriver for FailingWorker {
        fn acquire_holder(&mut self, _item: &SchedulerWorkItem) -> String {
            "failing-worker".to_string()
        }

        fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome {
            WorkerDispatchOutcome {
                work_item_id: item.id.clone(),
                worker_id: "failing-worker".to_string(),
                ok: false,
                message: "intentional worker failure".to_string(),
                evidence_refs: vec!["worker:failing-worker:error".to_string()],
            }
        }
    }

    #[test]
    fn worker_dispatch_failure_is_terminal_and_blocks_dependents() {
        let run_id = "run-worker-fail";
        let items = vec![
            make_work_item(run_id, "first", Vec::new()),
            make_work_item(run_id, "second", vec!["first".to_string()]),
        ];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = FailingWorker;
        let report = scheduler
            .dispatch_ready_batch(&mut store, &mut worker)
            .expect("dispatch ready batch");
        assert_eq!(report.attempted, 1);
        assert_eq!(report.completed, 0);
        assert_eq!(report.failed, 1);
        let items = scheduler.work_items();
        assert_eq!(
            items.iter().find(|item| item.id == "first").unwrap().state,
            WorkState::Failed
        );
        assert_eq!(
            items.iter().find(|item| item.id == "second").unwrap().state,
            WorkState::Ready
        );
        assert!(scheduler.ready_items().is_empty());
        let events = store.replay(run_id).expect("replay failure");
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::VerificationFailed
                    && event.payload["worker_ok"] == false)
        );
    }

    #[test]
    fn lease_heartbeat_extends_expiry_and_recovery_requeues_stale_items() {
        let run_id = "run-lease-recovery";
        let items = vec![
            make_work_item(run_id, "fresh", Vec::new()),
            make_work_item(run_id, "stale", Vec::new()),
        ];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .lease_ready_item(&mut store, "fresh", "lease-worker")
            .expect("lease fresh");
        scheduler
            .lease_ready_item(&mut store, "stale", "lease-worker")
            .expect("lease stale");
        let fresh_lease = scheduler.active_lease("fresh").expect("fresh lease");
        let heartbeat = scheduler
            .heartbeat_lease_with_fence(
                &mut store,
                "fresh",
                "lease-worker",
                &fresh_lease.id,
                20_000,
            )
            .expect("heartbeat fresh");
        assert_eq!(heartbeat.expires_at_ms, 50_000);
        let report = scheduler
            .expire_stale_leases(&mut store, 45_000)
            .expect("recover stale leases");
        assert_eq!(report.active_count, 1);
        assert_eq!(report.expired_count, 1);
        assert_eq!(report.active[0].work_item_id, "fresh");
        assert_eq!(report.expired[0].work_item_id, "stale");
        let items = scheduler.work_items();
        let fresh = items.iter().find(|item| item.id == "fresh").unwrap();
        let stale = items.iter().find(|item| item.id == "stale").unwrap();
        assert_eq!(fresh.state, WorkState::Leased);
        assert_eq!(
            fresh.lease.as_ref().unwrap().last_heartbeat_at_ms,
            Some(20_000)
        );
        assert_eq!(stale.state, WorkState::Ready);
        assert!(stale.lease.is_none());
        let events = store.replay(run_id).expect("replay leases");
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::LeaseHeartbeat
                    && event.payload["work_item_id"] == "fresh")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == EventKind::LeaseExpired
                    && event.payload["work_item_id"] == "stale"
                    && event.payload["to"] == "Ready")
        );
    }

    fn append_control_event(
        store: &mut EventStore,
        run_id: &str,
        kind: EventKind,
        target_id: Option<&str>,
        reason_code: &str,
    ) {
        let next_sequence = store.next_sequence(run_id).expect("next sequence");
        let mut payload = serde_json::json!({
            "message": "control",
            "reason_code": reason_code,
        });
        if let Some(target_id) = target_id {
            payload["target_id"] = Value::String(target_id.to_string());
            payload["work_item_id"] = Value::String(target_id.to_string());
        }
        let event = ExecutionEventEnvelope {
            schema: EXECUTION_EVENT_ENVELOPE_SCHEMA.to_string(),
            id: format!("evt-{run_id}-control-{next_sequence}"),
            run_id: run_id.to_string(),
            sequence: 0,
            occurred_at: "test-control".to_string(),
            actor: "test".to_string(),
            causation_id: None,
            correlation_id: target_id.map(str::to_string),
            kind,
            payload,
            sensitivity: Sensitivity::Public,
            evidence_refs: vec!["test:control".to_string()],
        };
        store.append_event(event).expect("append control event");
    }

    #[test]
    fn cancel_prevents_queued_dispatch() {
        // Acceptance criterion 2: a Cancel blocks new dispatch immediately and
        // transitions still-queued items to Cancelled; completed items stay
        // completed (partial run); the report reflects the split.
        let run_id = "run-cancel-dispatch";
        let items = vec![
            make_work_item(run_id, "done", Vec::new()),
            make_work_item(run_id, "queued-a", Vec::new()),
            make_work_item(run_id, "queued-b", Vec::new()),
        ];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");

        // Complete one item, then a cancel arrives in the durable mailbox.
        scheduler
            .lease_ready_item(&mut store, "done", "worker")
            .expect("lease done");
        scheduler
            .transition(&mut store, "done", WorkState::Running, Vec::new())
            .expect("run done");
        scheduler
            .transition(
                &mut store,
                "done",
                WorkState::Completed,
                vec!["proof".to_string()],
            )
            .expect("complete done");
        append_control_event(
            &mut store,
            run_id,
            EventKind::RunCancelled,
            None,
            "cancelled_by_user",
        );

        let mut worker = DeterministicWorker::new("worker");
        let controlled = scheduler
            .dispatch_until_idle_with_control(&mut store, &mut worker)
            .expect("controlled dispatch");

        assert_eq!(controlled.control_state, ExecutionControlState::Cancelled);
        // No new dispatch happened: nothing was attempted/completed by the worker.
        assert_eq!(controlled.report.attempted, 0);
        let cancel = controlled.cancel.expect("cancel report");
        assert_eq!(cancel.reason_code, "cancelled_by_user");
        assert_eq!(cancel.completed, vec!["done"]);
        let mut cancelled = cancel.cancelled.clone();
        cancelled.sort();
        assert_eq!(cancelled, vec!["queued-a", "queued-b"]);

        let items = scheduler.work_items();
        let state_of = |id: &str| {
            items
                .iter()
                .find(|item| item.id == id)
                .map(|item| item.state.clone())
                .unwrap()
        };
        assert_eq!(state_of("done"), WorkState::Completed);
        assert_eq!(state_of("queued-a"), WorkState::Cancelled);
        assert_eq!(state_of("queued-b"), WorkState::Cancelled);

        // The cancellation is durable: cancel transitions were appended.
        let events = store.replay(run_id).expect("replay");
        assert!(events.iter().any(|event| {
            event.kind == EventKind::WorkItemQueued
                && event.payload["to"] == "Cancelled"
                && event.payload["reason_code"] == "cancelled_by_user"
        }));
    }

    #[test]
    fn deterministic_worker_terminates_within_bound() {
        // The deterministic worker is retained for tests and must terminate
        // within a bounded number of dispatch transitions.
        let run_id = "run-deterministic-bound";
        let items: Vec<_> = (0..16)
            .map(|index| make_work_item(run_id, format!("wi-{index:02}"), Vec::new()))
            .collect();
        let item_count = items.len();
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        let mut worker = DeterministicWorker::new("bound-worker");
        let controlled = scheduler
            .dispatch_until_idle_with_control(&mut store, &mut worker)
            .expect("controlled dispatch terminates");
        assert_eq!(controlled.control_state, ExecutionControlState::Running);
        assert_eq!(controlled.report.completed, item_count);
        assert_eq!(controlled.report.failed, 0);
        assert!(
            controlled
                .snapshot
                .work_items
                .iter()
                .all(|item| item.state == WorkState::Completed)
        );
    }

    #[test]
    fn pause_blocks_new_dispatch_and_reports_true_state() {
        // Acceptance criterion 3: a Pause stops new dispatch; with the
        // synchronous worker nothing is mid-flight, so the run reaches the TRUE
        // paused state. Resume clears it and dispatch proceeds.
        let run_id = "run-pause";
        let items = vec![
            make_work_item(run_id, "first", Vec::new()),
            make_work_item(run_id, "second", Vec::new()),
        ];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        append_control_event(
            &mut store,
            run_id,
            EventKind::RunPaused,
            None,
            "paused_by_user",
        );

        let mut worker = DeterministicWorker::new("pause-worker");
        let paused = scheduler
            .dispatch_until_idle_with_control(&mut store, &mut worker)
            .expect("paused dispatch");
        assert_eq!(paused.control_state, ExecutionControlState::Paused);
        assert_eq!(paused.report.attempted, 0);
        assert!(
            scheduler
                .work_items()
                .iter()
                .all(|item| item.state == WorkState::Ready)
        );

        // Resume clears the pause and dispatch completes the work.
        append_control_event(
            &mut store,
            run_id,
            EventKind::RunResumed,
            None,
            "resumed_by_user",
        );
        let resumed = scheduler
            .dispatch_until_idle_with_control(&mut store, &mut worker)
            .expect("resumed dispatch");
        assert_eq!(resumed.control_state, ExecutionControlState::Running);
        assert_eq!(resumed.report.completed, 2);
    }

    #[test]
    fn steer_is_applied_or_rejected() {
        // Acceptance criterion 4: validate_steer_target returns a typed receipt:
        // Applied for a known steerable item, Rejected otherwise. We assert the
        // RETURNED receipt directly (not merely an appended event).
        let run_id = "run-steer";
        let items = vec![
            make_work_item(run_id, "steerable", Vec::new()),
            make_work_item(run_id, "finished", Vec::new()),
        ];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .lease_ready_item(&mut store, "finished", "worker")
            .expect("lease finished");
        scheduler
            .transition(&mut store, "finished", WorkState::Running, Vec::new())
            .expect("run finished");
        scheduler
            .transition(
                &mut store,
                "finished",
                WorkState::Completed,
                vec!["proof".to_string()],
            )
            .expect("complete finished");

        assert_eq!(
            scheduler.validate_steer_target("steerable"),
            SteerReceipt::Applied {
                target_id: "steerable".to_string()
            }
        );
        let rejected_unknown = scheduler.validate_steer_target("ghost");
        assert!(matches!(
            rejected_unknown,
            SteerReceipt::Rejected { reason, .. } if reason == "unknown_work_item"
        ));
        let rejected_terminal = scheduler.validate_steer_target("finished");
        assert!(matches!(
            rejected_terminal,
            SteerReceipt::Rejected { reason, .. } if reason.starts_with("work_item_terminal")
        ));
    }

    #[test]
    fn control_state_recovers_from_fresh_replay() {
        // Acceptance criterion 1 (recovery): folding the same control events from
        // a fresh replay yields the same control state. The events ARE the
        // durable mailbox, so it survives a restart.
        let run_id = "run-recovery";
        let items = vec![make_work_item(run_id, "wi", Vec::new())];
        let scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        append_control_event(&mut store, run_id, EventKind::RunPaused, None, "paused");
        append_control_event(&mut store, run_id, EventKind::RunResumed, None, "resumed");
        append_control_event(
            &mut store,
            run_id,
            EventKind::SteeringRequested,
            Some("wi"),
            "user_steering",
        );
        append_control_event(
            &mut store,
            run_id,
            EventKind::RunCancelled,
            None,
            "cancelled_by_user",
        );

        // First derivation via the scheduler.
        let live = scheduler.control_state(&store).expect("live control state");
        // Simulate a restart: a fresh replay folded independently.
        let replayed = store.replay(run_id).expect("replay");
        let recovered = ControlState::from_events(&replayed);
        assert_eq!(live, recovered);
        assert!(recovered.cancelled);
        assert!(!recovered.paused);
        assert_eq!(
            recovered.cancel_reason.as_deref(),
            Some("cancelled_by_user")
        );
        assert_eq!(recovered.pending_steer_targets, vec!["wi".to_string()]);

        // The mailbox commands fold to the same state.
        let mailbox = CommandMailbox::from_events(&replayed);
        assert_eq!(mailbox.control_state(), recovered);
    }

    #[test]
    fn lease_heartbeat_rejects_wrong_holder() {
        let run_id = "run-lease-holder";
        let items = vec![make_work_item(run_id, "item", Vec::new())];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .lease_ready_item(&mut store, "item", "holder-a")
            .expect("lease item");
        let error = scheduler
            .heartbeat_lease(&mut store, "item", "holder-b", 1_000)
            .expect_err("wrong holder rejected");
        assert!(matches!(error, SchedulerError::LeaseHolderMismatch { .. }));
    }

    #[test]
    fn lease_heartbeat_rejects_wrong_fence_token() {
        let run_id = "run-lease-heartbeat-fence";
        let items = vec![make_work_item(run_id, "item", Vec::new())];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .lease_ready_item(&mut store, "item", "holder-a")
            .expect("lease item");
        let active = scheduler.active_lease("item").expect("active lease");
        let error = scheduler
            .heartbeat_lease_with_fence(&mut store, "item", "holder-a", "stale-token", 1_000)
            .expect_err("wrong lease token rejected");
        assert!(matches!(
            error,
            SchedulerError::LeaseFenceMismatch { expected, actual, .. }
                if expected == active.id && actual == "stale-token"
        ));
        let item = scheduler
            .work_items()
            .into_iter()
            .find(|item| item.id == "item")
            .expect("item");
        assert_eq!(
            item.lease.and_then(|lease| lease.last_heartbeat_at_ms),
            None
        );
    }

    #[test]
    fn fenced_transition_rejects_wrong_lease_token() {
        let run_id = "run-fence-token";
        let items = vec![make_work_item(run_id, "item", Vec::new())];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .lease_ready_item(&mut store, "item", "holder-a")
            .expect("lease item");
        let active = scheduler.active_lease("item").expect("active lease");
        let error = scheduler
            .transition_with_fence(
                &mut store,
                "item",
                WorkState::Running,
                "holder-a",
                "stale-token",
                vec!["test:stale-token".to_string()],
            )
            .expect_err("wrong token rejected");
        assert!(matches!(
            error,
            SchedulerError::LeaseFenceMismatch { expected, actual, .. }
                if expected == active.id && actual == "stale-token"
        ));
        let item = scheduler
            .work_items()
            .into_iter()
            .find(|item| item.id == "item")
            .expect("item");
        assert_eq!(item.state, WorkState::Leased);
        assert!(
            !store
                .replay(run_id)
                .expect("replay")
                .iter()
                .any(|event| event.kind == EventKind::WorkItemRunning
                    && event.payload["to"] == "Running")
        );
    }

    #[test]
    fn reacquired_lease_gets_new_fence_and_rejects_stale_worker() {
        let run_id = "run-reacquire-fence";
        let items = vec![make_work_item(run_id, "item", Vec::new())];
        let mut scheduler = DurableScheduler::new(run_id, items, SchedulerConfig::default());
        let mut store = EventStore::open_memory().expect("event store");
        scheduler
            .lease_ready_item(&mut store, "item", "worker")
            .expect("first lease");
        let stale = scheduler.active_lease("item").expect("stale lease");

        scheduler
            .expire_stale_leases(
                &mut store,
                stale
                    .acquired_at_ms
                    .saturating_add(stale.ttl_ms)
                    .saturating_add(1),
            )
            .expect("expire stale lease");
        scheduler
            .lease_ready_item(&mut store, "item", "worker")
            .expect("second lease");
        let current = scheduler.active_lease("item").expect("current lease");
        assert_ne!(stale.id, current.id);

        let stale_error = scheduler
            .transition_with_fence(
                &mut store,
                "item",
                WorkState::Running,
                "worker",
                &stale.id,
                vec!["test:stale-worker".to_string()],
            )
            .expect_err("stale lease token rejected");
        assert!(matches!(
            stale_error,
            SchedulerError::LeaseFenceMismatch { expected, actual, .. }
                if expected == current.id && actual == stale.id
        ));

        scheduler
            .transition_with_fence(
                &mut store,
                "item",
                WorkState::Running,
                "worker",
                &current.id,
                vec!["test:current-worker".to_string()],
            )
            .expect("current lease can run");
        scheduler
            .transition_with_fence(
                &mut store,
                "item",
                WorkState::Completed,
                "worker",
                &current.id,
                vec!["test:current-worker".to_string()],
            )
            .expect("current lease can complete");
    }
}
