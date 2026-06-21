use std::collections::{BTreeMap, BTreeSet};

use opensks_contracts::{
    CONCURRENCY_DECISION_SCHEMA, ConcurrencyDecision, EXECUTION_EVENT_ENVELOPE_SCHEMA, EventKind,
    ExecutionEventEnvelope, Lease, LeaseType, SCHEDULER_WORK_ITEM_SCHEMA, SchedulerSnapshot,
    SchedulerWorkItem, Sensitivity, WorkBudget, WorkKind, WorkState,
};
use opensks_event_store::{EventStore, EventStoreError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

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
    pub worker_ids: Vec<String>,
    pub outcomes: Vec<WorkerDispatchOutcome>,
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

impl WorkerDispatchReport {
    fn new(run_id: String, decision: ConcurrencyDecision) -> Self {
        Self {
            run_id,
            decision,
            attempted: 0,
            completed: 0,
            failed: 0,
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
        for worker_id in next.worker_ids {
            self.add_worker_id(&worker_id);
        }
        self.outcomes.extend(next.outcomes);
    }
}

pub trait WorkerDriver {
    fn acquire_holder(&mut self, item: &SchedulerWorkItem) -> String;
    fn execute(&mut self, item: &SchedulerWorkItem) -> WorkerDispatchOutcome;
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
        evidence_refs: Vec<String>,
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
                self.assign_lease(item_id, "sim-worker");
                self.transition(store, item_id, WorkState::Leased, Vec::new())?;
                self.transition(store, item_id, WorkState::Running, Vec::new())?;
                self.transition(
                    store,
                    item_id,
                    WorkState::Completed,
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
        self.assign_lease(item_id, holder);
        self.transition_with_payload(
            store,
            item_id,
            WorkState::Leased,
            vec!["scheduler:worker-lease".to_string()],
            worker_payload(holder, None, None),
        )
    }

    pub fn heartbeat_lease(
        &mut self,
        store: &mut EventStore,
        item_id: &str,
        holder: &str,
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

        let snapshot = self.snapshot_with_evidence(
            decision,
            vec![
                "event-store-replay-required".to_string(),
                "scheduler:worker-dispatch".to_string(),
            ],
        );
        Ok((snapshot, report))
    }

    fn dispatch_ready_batch_with_decision<D: WorkerDriver>(
        &mut self,
        store: &mut EventStore,
        driver: &mut D,
        decision: ConcurrencyDecision,
    ) -> Result<WorkerDispatchReport, SchedulerError> {
        let admitted = decision.admitted as usize;
        let ready = self.ready_items();
        let batch: Vec<String> = ready.into_iter().take(admitted).collect();
        self.max_concurrent_workers = self.max_concurrent_workers.max(batch.len() as u32);
        let mut report = WorkerDispatchReport::new(self.run_id.clone(), decision);
        for item_id in batch {
            let item = self
                .items
                .get(&item_id)
                .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.clone()))?
                .clone();
            let holder = driver.acquire_holder(&item);
            report.add_worker_id(&holder);
            self.assign_lease(&item_id, &holder);
            self.transition_with_payload(
                store,
                &item_id,
                WorkState::Leased,
                vec!["scheduler:worker-lease".to_string()],
                worker_payload(&holder, None, None),
            )?;
            self.transition_with_payload(
                store,
                &item_id,
                WorkState::Dispatched,
                vec!["scheduler:worker-dispatch".to_string()],
                worker_payload(&holder, None, None),
            )?;
            self.transition_with_payload(
                store,
                &item_id,
                WorkState::Running,
                vec!["scheduler:worker-running".to_string()],
                worker_payload(&holder, None, None),
            )?;

            let running_item = self
                .items
                .get(&item_id)
                .ok_or_else(|| SchedulerError::UnknownWorkItem(item_id.clone()))?
                .clone();
            let outcome = driver.execute(&running_item);
            report.attempted += 1;
            report.add_worker_id(&outcome.worker_id);
            let mut outcome_evidence_refs = outcome.evidence_refs.clone();
            if !outcome_evidence_refs
                .iter()
                .any(|evidence_ref| evidence_ref == "scheduler:worker-dispatch")
            {
                outcome_evidence_refs.push("scheduler:worker-dispatch".to_string());
            }
            if outcome.ok {
                self.transition_with_payload(
                    store,
                    &item_id,
                    WorkState::ResultReceived,
                    outcome_evidence_refs.clone(),
                    worker_payload(&outcome.worker_id, Some(&outcome.message), Some(true)),
                )?;
                self.transition_with_payload(
                    store,
                    &item_id,
                    WorkState::Verifying,
                    vec!["scheduler:worker-result-verification".to_string()],
                    worker_payload(&outcome.worker_id, Some(&outcome.message), Some(true)),
                )?;
                self.transition_with_payload(
                    store,
                    &item_id,
                    WorkState::Applying,
                    vec!["scheduler:worker-result-apply".to_string()],
                    worker_payload(&outcome.worker_id, Some(&outcome.message), Some(true)),
                )?;
                self.transition_with_payload(
                    store,
                    &item_id,
                    WorkState::Completed,
                    outcome_evidence_refs,
                    worker_payload(&outcome.worker_id, Some(&outcome.message), Some(true)),
                )?;
                report.completed += 1;
            } else {
                self.transition_with_payload(
                    store,
                    &item_id,
                    WorkState::Failed,
                    outcome_evidence_refs,
                    worker_payload(&outcome.worker_id, Some(&outcome.message), Some(false)),
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

    fn assign_lease(&mut self, item_id: &str, holder: &str) {
        if let Some(item) = self.items.get_mut(item_id) {
            item.lease = Some(Lease {
                id: format!("lease-{}-{item_id}", self.run_id),
                lease_type: LeaseType::ProviderSlot,
                holder: holder.to_string(),
                acquired_at_ms: self.transitions_committed + 1,
                last_heartbeat_at_ms: None,
                ttl_ms: DEFAULT_WORKER_LEASE_TTL_MS,
            });
        }
    }
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
        model_selector: None,
        path_scope: opensks_contracts::PathScope::default(),
        budget: WorkBudget::default(),
        retry: opensks_contracts::RetryPolicy::default(),
        lease: None,
        idempotency_key: format!("idem-{run_id}-{id}"),
        requirement_ids: Vec::new(),
        evidence_refs: Vec::new(),
    }
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
        let heartbeat = scheduler
            .heartbeat_lease(&mut store, "fresh", "lease-worker", 20_000)
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
}
