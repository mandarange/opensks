// EventBatcher.swift — PR-043. Coalesces a high-rate event stream into AT MOST
// one UI update per frame/interval so 1,000s of events/sec do not cause 1,000s
// of SwiftUI invalidations.
//
// The hardening invariant: a `@Published` store driven directly by a live
// engine stream (e.g. `PipelineProjectionStore.ingest` per event) republishes
// once per event. Under a burst that is one objectWillChange + one diff per
// event — quadratic churn in the worst case. The batcher inverts this: the
// producer side updates an in-memory model as fast as events arrive (cheap,
// no view involvement), but the UI-facing flush fires at most once per
// `interval`, delivering only the LATEST coalesced state.
//
// BOUNDEDNESS (the load-bearing property): the batcher never retains a backlog
// of events. It holds exactly ONE pending value (the latest), overwriting it on
// each `submit`. Memory is therefore O(1) in stream length — 10,000 submits
// retain one value, not 10,000. The flush count is bounded by elapsed-time /
// interval, NOT by the number of submits.
//
// This is deliberately a CADisplayLink-free, timer-free *logical* coalescer so
// it is deterministic and testable: `submit` records the latest value and arms
// a single pending flush; `flushIfNeeded` (driven by a tick — a Task, a
// DisplayLink, an onChange, or a test) delivers the coalesced value and
// disarms. No unbounded queue, no per-event view work.

import Foundation

/// Coalesces rapid `submit(_:)` calls into at-most-one `onFlush` delivery per
/// scheduling tick, always preserving the latest submitted value. Generic over
/// the coalesced value type. `@MainActor` because it drives UI republish.
@MainActor
final class EventBatcher<Value> {
    /// Delivered with the latest coalesced value on each flush. Wire this to the
    /// store's republish (e.g. assign to a `@Published` property).
    var onFlush: (Value) -> Void

    /// Minimum wall-clock spacing between flushes. A burst inside one interval
    /// collapses to a single flush carrying the last value. ~16ms ≈ one 60Hz
    /// frame; callers may widen it to trade latency for fewer invalidations.
    let interval: TimeInterval

    /// The single pending (latest) value. Exactly one slot — never a backlog.
    private var pending: Value?
    /// True once a flush has been scheduled and not yet delivered.
    private var isScheduled = false
    /// Monotonic count of flushes actually delivered (for tests / metrics).
    private(set) var flushCount = 0
    /// Monotonic count of values submitted (for tests / metrics).
    private(set) var submitCount = 0
    /// The scheduling clock — injectable so tests are deterministic.
    private let now: () -> TimeInterval
    /// Wall-clock time of the last delivered flush, for interval gating.
    private var lastFlushAt: TimeInterval?
    /// Whether an async auto-flush task drives delivery. Off in unit tests that
    /// drive `flushIfNeeded()` manually for determinism.
    private let autoFlush: Bool

    init(
        interval: TimeInterval = 1.0 / 60.0,
        autoFlush: Bool = true,
        now: @escaping () -> TimeInterval = { Date().timeIntervalSinceReferenceDate },
        onFlush: @escaping (Value) -> Void = { _ in }
    ) {
        self.interval = interval
        self.autoFlush = autoFlush
        self.now = now
        self.onFlush = onFlush
    }

    /// Record the latest value. O(1): overwrites the single pending slot, so a
    /// burst of N submits retains ONE value, never N. Arms a single pending
    /// flush; subsequent submits before that flush do not schedule extra work.
    func submit(_ value: Value) {
        submitCount += 1
        pending = value
        guard !isScheduled else { return }
        isScheduled = true
        if autoFlush { scheduleAutoFlush() }
    }

    /// Deliver the latest pending value IF one is pending and the interval has
    /// elapsed since the last flush. Returns true if a flush was delivered.
    /// This is the tick entry point — a DisplayLink, a Task loop, a SwiftUI
    /// `onChange`, or a test calls it. Coalescing guarantees: regardless of how
    /// many `submit`s happened, this delivers ONE value (the latest).
    @discardableResult
    func flushIfNeeded(force: Bool = false) -> Bool {
        guard pending != nil else { isScheduled = false; return false }
        if !force, let last = lastFlushAt, now() - last < interval {
            // Too soon — stay armed; the next eligible tick delivers.
            return false
        }
        return deliver()
    }

    /// Force-deliver the latest pending value immediately (e.g. on a terminal
    /// frame, a route change, or teardown) so no final state is left unshown.
    @discardableResult
    func flushNow() -> Bool {
        guard pending != nil else { isScheduled = false; return false }
        return deliver()
    }

    /// Drop the pending value WITHOUT delivering it (e.g. when the owning view is
    /// torn down and the value is now irrelevant). Keeps the batcher bounded.
    func cancelPending() {
        pending = nil
        isScheduled = false
    }

    // MARK: - Internals

    private func deliver() -> Bool {
        guard let value = pending else { isScheduled = false; return false }
        pending = nil
        isScheduled = false
        lastFlushAt = now()
        flushCount += 1
        onFlush(value)
        return true
    }

    /// Schedule a single async flush after `interval`. Re-arms only while a
    /// value remains pending, so an idle batcher does no work.
    private func scheduleAutoFlush() {
        let delayNanos = UInt64(max(0, interval) * 1_000_000_000)
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: delayNanos)
            guard let self else { return }
            // Force here: the timer IS the interval gate, so deliver the latest.
            let delivered = self.flushNow()
            // If more arrived during the sleep, `submit` already re-armed; if not,
            // and a value is still somehow pending (delivered==false path), the
            // next submit re-schedules. Nothing unbounded is retained either way.
            _ = delivered
        }
    }
}
