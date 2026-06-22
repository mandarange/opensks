//! Bounded event batcher (PR-043).
//!
//! [`BoundedBatcher`] coalesces a high-rate event stream into order-preserving
//! batches so a downstream consumer (the UI, a projection) is never flooded one
//! event at a time. It holds at most `max_pending` events in flight; once that
//! many accumulate without a flush it applies an EXPLICIT, COUNTED overflow
//! policy (drop-oldest or drop-newest) rather than dropping events silently or
//! growing without bound. A flush emits a `Batch` of at most `max_batch` events
//! in arrival order.
//!
//! Order guarantee: events that are retained are emitted in the exact order
//! they were pushed. Loss guarantee: `pushed == emitted + dropped` always, and
//! `dropped` only ever advances through the counted overflow policy.

use std::collections::VecDeque;

/// What the batcher does when `max_pending` in-flight events would be exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy {
    /// Drop the oldest buffered event to admit the newest (keep latest state).
    DropOldest,
    /// Drop the incoming event (keep the earliest history).
    DropNewest,
}

/// Configuration for [`BoundedBatcher`].
#[derive(Debug, Clone, Copy)]
pub struct BatcherConfig {
    /// Auto-flush threshold: `push` returns a [`Batch`] once this many events
    /// are buffered (clamped to `1 ..= max_pending`).
    pub max_batch: usize,
    /// Hard cap on events buffered in flight (clamped to >= 1). The buffer NEVER
    /// exceeds this; overflow past it applies the [`OverflowPolicy`].
    pub max_pending: usize,
    /// What to do when a push would exceed `max_pending`.
    pub overflow: OverflowPolicy,
    /// When `true`, `push` auto-flushes a batch at `max_batch`. When `false`,
    /// the caller drains via [`BoundedBatcher::flush`] / [`BoundedBatcher::drain`]
    /// on its own cadence, and the in-flight cap is held purely by the counted
    /// overflow policy.
    pub auto_flush: bool,
}

impl BatcherConfig {
    /// A sane default: auto-flushing small batches, modest in-flight cap,
    /// drop-oldest overflow.
    pub fn coalescing(max_batch: usize, max_pending: usize) -> Self {
        Self {
            max_batch,
            max_pending,
            overflow: OverflowPolicy::DropOldest,
            auto_flush: true,
        }
    }
}

/// An emitted batch: a contiguous, order-preserving slice of the input stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch<E> {
    pub events: Vec<E>,
}

impl<E> Batch<E> {
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Coalesces a high-rate event stream into bounded, order-preserving batches.
pub struct BoundedBatcher<E> {
    max_batch: usize,
    max_pending: usize,
    overflow: OverflowPolicy,
    auto_flush: bool,
    pending: VecDeque<E>,
    pushed: u64,
    emitted: u64,
    dropped: u64,
    peak_pending: usize,
}

impl<E> BoundedBatcher<E> {
    /// Create a batcher from a config. `max_pending` is clamped to >= 1 (the
    /// hard in-flight cap) and `max_batch` to `1 ..= max_pending` so a batch can
    /// never be larger than the buffer that produced it.
    pub fn new(config: BatcherConfig) -> Self {
        let max_pending = config.max_pending.max(1);
        let max_batch = config.max_batch.clamp(1, max_pending);
        Self {
            max_batch,
            max_pending,
            overflow: config.overflow,
            auto_flush: config.auto_flush,
            pending: VecDeque::with_capacity(max_pending),
            pushed: 0,
            emitted: 0,
            dropped: 0,
            peak_pending: 0,
        }
    }

    /// Events buffered in flight right now (never exceeds `max_pending`).
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Hard cap on in-flight events.
    pub fn max_pending(&self) -> usize {
        self.max_pending
    }

    /// Total events handed to [`push`](Self::push).
    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    /// Total events delivered through flushed batches.
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    /// Total events dropped by the explicit overflow policy.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// High-water mark of in-flight events; never exceeds `max_pending`.
    pub fn peak_pending(&self) -> usize {
        self.peak_pending
    }

    /// Push one event. If admitting it would exceed `max_pending`, the
    /// configured overflow policy drops exactly one event (counted in
    /// `dropped`). Returns a full [`Batch`] when the in-flight buffer reaches
    /// `max_batch`, otherwise `None`.
    pub fn push(&mut self, event: E) -> Option<Batch<E>> {
        self.pushed += 1;
        if self.pending.len() >= self.max_pending {
            match self.overflow {
                OverflowPolicy::DropOldest => {
                    self.pending.pop_front();
                    self.dropped += 1;
                    self.pending.push_back(event);
                }
                OverflowPolicy::DropNewest => {
                    self.dropped += 1;
                    // Incoming event is dropped; buffer unchanged.
                }
            }
        } else {
            self.pending.push_back(event);
        }
        self.peak_pending = self.peak_pending.max(self.pending.len());

        if self.auto_flush && self.pending.len() >= self.max_batch {
            self.flush()
        } else {
            None
        }
    }

    /// Drain up to `max_batch` events into a batch, preserving arrival order.
    /// Returns `None` when nothing is pending.
    pub fn flush(&mut self) -> Option<Batch<E>> {
        if self.pending.is_empty() {
            return None;
        }
        let take = self.pending.len().min(self.max_batch);
        let mut events = Vec::with_capacity(take);
        for _ in 0..take {
            if let Some(event) = self.pending.pop_front() {
                events.push(event);
            }
        }
        self.emitted += events.len() as u64;
        Some(Batch { events })
    }

    /// Drain everything still pending into a final batch (which may exceed
    /// `max_batch`). Use once the input stream is closed so no event is left
    /// behind. Returns `None` when nothing is pending.
    pub fn drain(&mut self) -> Option<Batch<E>> {
        if self.pending.is_empty() {
            return None;
        }
        let events: Vec<E> = self.pending.drain(..).collect();
        self.emitted += events.len() as u64;
        Some(Batch { events })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batcher_coalesces_100k_events_with_bounded_memory_and_no_loss() {
        // High-rate budget proof: 100k events through a batcher whose in-flight
        // buffer is capped at 512 and whose batch size is 128. The in-flight
        // buffer must never exceed its cap, every event must be accounted for
        // (emitted + dropped == pushed), and with timely flushing nothing is
        // dropped (drop is an explicit, counted policy — zero here).
        let total = 100_000u64;
        let mut batcher: BoundedBatcher<u64> =
            BoundedBatcher::new(BatcherConfig::coalescing(128, 512));
        let mut collected: Vec<u64> = Vec::new();
        for event in 0..total {
            if let Some(batch) = batcher.push(event) {
                assert!(batch.len() <= 128, "batch exceeded max_batch");
                collected.extend(batch.events);
            }
            // The in-flight buffer is bounded on EVERY push regardless of how
            // many events have streamed through.
            assert!(
                batcher.pending_len() <= batcher.max_pending(),
                "pending exceeded cap at event {event}"
            );
        }
        if let Some(batch) = batcher.drain() {
            collected.extend(batch.events);
        }

        assert!(batcher.peak_pending() <= 512, "peak pending exceeded cap");
        assert_eq!(batcher.dropped(), 0, "no silent or policy drops expected");
        assert_eq!(batcher.pushed(), total);
        assert_eq!(batcher.emitted(), total);
        // Order is preserved end to end and nothing is lost.
        assert_eq!(collected.len(), total as usize);
        assert!(collected.iter().enumerate().all(|(i, &e)| e == i as u64));
    }

    #[test]
    fn overflow_drop_oldest_is_explicit_and_counted() {
        // If the consumer never flushes, the in-flight cap is still honored and
        // overflow is an explicit, counted drop — never silent unbounded growth.
        let mut batcher: BoundedBatcher<u32> = BoundedBatcher::new(BatcherConfig {
            max_batch: 4,
            max_pending: 4,
            overflow: OverflowPolicy::DropOldest,
            auto_flush: false, // caller drains manually; cap held by overflow
        });
        for event in 0..10u32 {
            batcher.push(event);
            assert!(batcher.pending_len() <= 4);
        }
        // 10 pushed, cap 4, so 6 oldest dropped; the final 4 survive in order.
        assert_eq!(batcher.dropped(), 6);
        let batch = batcher.drain().expect("final drain");
        assert_eq!(batch.events, vec![6, 7, 8, 9]);
        assert_eq!(batcher.pushed(), 10);
        assert_eq!(batcher.emitted() + batcher.dropped(), batcher.pushed());
    }

    #[test]
    fn overflow_drop_newest_keeps_earliest() {
        let mut batcher: BoundedBatcher<u32> = BoundedBatcher::new(BatcherConfig {
            max_batch: 3,
            max_pending: 3,
            overflow: OverflowPolicy::DropNewest,
            auto_flush: false,
        });
        for event in 0..10u32 {
            batcher.push(event);
        }
        assert_eq!(batcher.dropped(), 7);
        let batch = batcher.drain().expect("final drain");
        assert_eq!(batch.events, vec![0, 1, 2]);
    }

    #[test]
    fn flush_emits_in_order_partial_batches() {
        let mut batcher: BoundedBatcher<u8> = BoundedBatcher::new(BatcherConfig::coalescing(4, 16));
        batcher.push(1);
        batcher.push(2);
        let batch = batcher.flush().expect("partial flush");
        assert_eq!(batch.events, vec![1, 2]);
        assert!(batcher.flush().is_none());
    }
}
