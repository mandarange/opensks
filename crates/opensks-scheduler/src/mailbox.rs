//! Durable command mailbox for scheduler run control.
//!
//! Control commands (cancel / pause / resume / steer) are not stored in a new
//! persistence layer. Instead they live as ordinary control events in the
//! event store (`RunCancelled` / `RunPaused` / `RunResumed` / `SteeringRequested`).
//! The events ARE the mailbox: replaying a run's events and folding them into a
//! [`ControlState`] yields the current control intent, and the same fold over a
//! fresh replay recovers the identical state after a restart.

use opensks_contracts::{EventKind, ExecutionEventEnvelope};
use serde::{Deserialize, Serialize};

/// A single pending control command derived from the run's control events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchedulerCommand {
    /// Cancel the run: block new dispatch and cancel still-queued work.
    Cancel { reason_code: String },
    /// Pause the run: stop new dispatch and quiesce to `paused`.
    Pause { reason_code: String },
    /// Resume a previously paused run.
    Resume { reason_code: String },
    /// Steer a specific work item with a (redacted) message.
    Steer {
        target_id: String,
        message: String,
        reason_code: String,
    },
}

/// The folded control intent for a run, derived from its control events.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlState {
    /// True once a `RunCancelled` event has been observed.
    pub cancelled: bool,
    /// True while paused; cleared by `RunResumed`.
    pub paused: bool,
    /// Reason code carried by the most recent cancel command, if any.
    pub cancel_reason: Option<String>,
    /// Work-item targets that have an outstanding steer request.
    pub pending_steer_targets: Vec<String>,
}

impl ControlState {
    /// Fold a run's control events (in sequence order) into the current state.
    pub fn from_events(events: &[ExecutionEventEnvelope]) -> Self {
        let mut state = ControlState::default();
        for event in events {
            state.apply(event);
        }
        state
    }

    /// Apply a single event to the control state. Non-control events are ignored.
    pub fn apply(&mut self, event: &ExecutionEventEnvelope) {
        match event.kind {
            EventKind::RunCancelled => {
                self.cancelled = true;
                self.cancel_reason = reason_code(event).or_else(|| Some("cancelled".to_string()));
            }
            EventKind::RunPaused => {
                self.paused = true;
            }
            EventKind::RunResumed => {
                self.paused = false;
            }
            EventKind::SteeringRequested => {
                if let Some(target) = target_id(event) {
                    if !self.pending_steer_targets.contains(&target) {
                        self.pending_steer_targets.push(target);
                    }
                }
            }
            _ => {}
        }
    }

    /// True when new dispatch must be blocked (cancelled or paused).
    pub fn blocks_new_dispatch(&self) -> bool {
        self.cancelled || self.paused
    }
}

/// A durable mailbox of pending control commands for a run.
///
/// The mailbox is derived purely from replayed control events, so it is durable
/// and recovers after a restart without any extra persistence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMailbox {
    pub commands: Vec<SchedulerCommand>,
}

impl CommandMailbox {
    /// Derive the ordered pending commands by replaying a run's events.
    pub fn from_events(events: &[ExecutionEventEnvelope]) -> Self {
        let mut commands = Vec::new();
        for event in events {
            match event.kind {
                EventKind::RunCancelled => commands.push(SchedulerCommand::Cancel {
                    reason_code: reason_code(event).unwrap_or_else(|| "cancelled".to_string()),
                }),
                EventKind::RunPaused => commands.push(SchedulerCommand::Pause {
                    reason_code: reason_code(event).unwrap_or_else(|| "paused".to_string()),
                }),
                EventKind::RunResumed => commands.push(SchedulerCommand::Resume {
                    reason_code: reason_code(event).unwrap_or_else(|| "resumed".to_string()),
                }),
                EventKind::SteeringRequested => {
                    if let Some(target_id) = target_id(event) {
                        commands.push(SchedulerCommand::Steer {
                            target_id,
                            message: message(event).unwrap_or_default(),
                            reason_code: reason_code(event)
                                .unwrap_or_else(|| "user_steering".to_string()),
                        });
                    }
                }
                _ => {}
            }
        }
        Self { commands }
    }

    /// Fold the mailbox commands into the current control state.
    pub fn control_state(&self) -> ControlState {
        let mut state = ControlState::default();
        for command in &self.commands {
            match command {
                SchedulerCommand::Cancel { reason_code } => {
                    state.cancelled = true;
                    state.cancel_reason = Some(reason_code.clone());
                }
                SchedulerCommand::Pause { .. } => state.paused = true,
                SchedulerCommand::Resume { .. } => state.paused = false,
                SchedulerCommand::Steer { target_id, .. } => {
                    if !state.pending_steer_targets.contains(target_id) {
                        state.pending_steer_targets.push(target_id.clone());
                    }
                }
            }
        }
        state
    }
}

fn reason_code(event: &ExecutionEventEnvelope) -> Option<String> {
    event
        .payload
        .get("reason_code")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn target_id(event: &ExecutionEventEnvelope) -> Option<String> {
    event
        .payload
        .get("target_id")
        .or_else(|| event.payload.get("work_item_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| event.correlation_id.clone())
}

fn message(event: &ExecutionEventEnvelope) -> Option<String> {
    event
        .payload
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}
