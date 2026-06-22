//! High-rate stress harness (PR-043).
//!
//! [`run_stress`] pushes `events` synthetic events through a [`BoundedBatcher`]
//! and a [`BoundedLruCache`], while a [`ProcessSupervisor`] spawns and reaps a
//! fixed pool of short-lived bookkeeping children. It asserts a single bounded
//! memory budget: the peak number of simultaneously RETAINED items (cache
//! entries + in-flight batch events) never exceeds the configured cap, no
//! matter how large the input stream is. The result is a deterministic
//! [`PerfStressReport`] suitable for a CLI to print and a test to assert.

use opensks_contracts::{PERF_STRESS_REPORT_SCHEMA, PerfStressReport};

use crate::batcher::{BatcherConfig, BoundedBatcher};
use crate::cache::BoundedLruCache;
use crate::supervisor::{CancelToken, ProcessSupervisor, Reapable};

/// Knobs for the stress harness. Defaults give a tight retention budget so the
/// bound is meaningfully tested against a large input.
#[derive(Debug, Clone, Copy)]
pub struct StressConfig {
    /// Number of synthetic events to process.
    pub events: u64,
    /// LRU cache capacity (hard cap on retained cache entries).
    pub cache_capacity: usize,
    /// Batch flush threshold.
    pub max_batch: usize,
    /// Hard cap on in-flight (buffered) events.
    pub max_pending: usize,
    /// Number of supervised bookkeeping children to spawn and reap.
    pub supervised_children: u64,
}

impl Default for StressConfig {
    fn default() -> Self {
        Self {
            events: 100_000,
            cache_capacity: 1_024,
            max_batch: 256,
            max_pending: 1_024,
            supervised_children: 16,
        }
    }
}

impl StressConfig {
    /// The retention cap the harness holds the run to: the most items that can
    /// be simultaneously retained is the cache capacity plus a single in-flight
    /// batch's worth of events (a flush never lets pending exceed
    /// `max_pending`, and a batch carries at most `max_batch`).
    pub fn retention_cap(&self) -> u64 {
        self.cache_capacity as u64 + self.max_pending.max(self.max_batch) as u64
    }
}

/// A trivial deterministic child for the supervised bookkeeping pool used by the
/// harness — keeps the stress run free of real process-spawn latency while still
/// exercising the supervisor's spawn/track/reap registry and leak counter.
struct CounterChild {
    reaped: bool,
}

impl CounterChild {
    fn new() -> Self {
        Self { reaped: false }
    }
}

impl Reapable for CounterChild {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn wait(&mut self) -> std::io::Result<()> {
        self.reaped = true;
        Ok(())
    }
}

/// Run the stress harness and return its report. The run completes (returns)
/// only after every supervised child has been reaped and every event has been
/// drained from the batcher.
pub fn run_stress(config: StressConfig) -> PerfStressReport {
    let cancel = CancelToken::new();
    let mut supervisor: ProcessSupervisor<CounterChild> = ProcessSupervisor::with_observer(&cancel);
    for _ in 0..config.supervised_children {
        supervisor.track("perf-bookkeeping", CounterChild::new());
    }

    let mut cache: BoundedLruCache<u64, u64> = BoundedLruCache::new(config.cache_capacity);
    let mut batcher: BoundedBatcher<u64> = BoundedBatcher::new(BatcherConfig::coalescing(
        config.max_batch,
        config.max_pending,
    ));

    let retention_cap = config.retention_cap();
    let mut peak_retained: u64 = 0;

    // The hot loop: every event is coalesced into a batch; each flushed batch is
    // folded into the bounded cache. Retained items at any instant = cache.len()
    // + events currently buffered in the batcher.
    let process_batch = |cache: &mut BoundedLruCache<u64, u64>, events: &[u64]| {
        for &event in events {
            // Key space deliberately exceeds the cache capacity so the LRU
            // genuinely evicts under load.
            cache.insert(event, event.wrapping_mul(2654435761));
        }
    };

    for event in 0..config.events {
        if let Some(batch) = batcher.push(event) {
            process_batch(&mut cache, &batch.events);
        }
        let retained = cache.len() as u64 + batcher.pending_len() as u64;
        peak_retained = peak_retained.max(retained);
    }
    if let Some(batch) = batcher.drain() {
        process_batch(&mut cache, &batch.events);
    }

    // Deterministically reap the supervised pool; the run is not "done" until
    // every child is reaped and the leak counter is zero.
    supervisor
        .reap_all()
        .expect("supervised bookkeeping children must reap cleanly");

    let processed = batcher.emitted();
    let dropped = batcher.dropped();
    let children_spawned = supervisor.spawned();
    let children_reaped = supervisor.reaped();
    let leaked_handles = supervisor.leaked_handles() as u64;

    let within_budget = peak_retained <= retention_cap
        && processed + dropped == config.events
        && children_reaped == children_spawned
        && leaked_handles == 0;

    PerfStressReport {
        schema: PERF_STRESS_REPORT_SCHEMA.to_string(),
        events: config.events,
        processed,
        dropped,
        retention_cap,
        peak_retained,
        children_spawned,
        children_reaped,
        leaked_handles,
        within_budget,
        evidence_refs: vec![
            "perf:bounded-lru-cache".to_string(),
            "perf:bounded-event-batcher".to_string(),
            "perf:process-supervisor-reap".to_string(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stress_100k_events_stays_within_retention_budget() {
        // The headline budget proof: 100k events processed, peak retained items
        // bounded by the configured cap regardless of input size, nothing lost
        // (processed + dropped == events), every supervised child reaped, zero
        // leaked handles. The single `within_budget` invariant captures it all.
        let config = StressConfig::default();
        let report = run_stress(config);

        assert_eq!(report.events, 100_000);
        assert_eq!(
            report.processed + report.dropped,
            report.events,
            "no event may be lost silently"
        );
        assert_eq!(report.dropped, 0, "timely flushing drops nothing");
        assert!(
            report.peak_retained <= report.retention_cap,
            "peak retained {} exceeded cap {}",
            report.peak_retained,
            report.retention_cap
        );
        assert_eq!(report.children_spawned, report.children_reaped);
        assert_eq!(report.leaked_handles, 0);
        assert!(report.within_budget, "stress run must be within budget");
    }

    #[test]
    fn budget_holds_as_input_scales_but_cap_stays_fixed() {
        // Same fixed caps, 10x the input: peak retained must NOT scale with the
        // input — that is the whole point of the bound.
        let small = run_stress(StressConfig {
            events: 10_000,
            ..StressConfig::default()
        });
        let large = run_stress(StressConfig {
            events: 200_000,
            ..StressConfig::default()
        });
        assert_eq!(small.retention_cap, large.retention_cap);
        assert!(small.peak_retained <= small.retention_cap);
        assert!(large.peak_retained <= large.retention_cap);
        // The 20x larger input does not raise the retention high-water mark
        // beyond the fixed cap.
        assert!(large.peak_retained <= large.retention_cap);
        assert!(large.within_budget && small.within_budget);
    }

    #[test]
    fn tiny_caps_force_eviction_and_drops_are_counted_not_silent() {
        // With a cache far smaller than the key space the LRU must evict, and
        // accounting still balances (processed + dropped == events).
        let report = run_stress(StressConfig {
            events: 50_000,
            cache_capacity: 64,
            max_batch: 32,
            max_pending: 128,
            supervised_children: 4,
        });
        assert_eq!(report.processed + report.dropped, report.events);
        assert!(report.peak_retained <= report.retention_cap);
        assert!(report.within_budget);
    }
}
