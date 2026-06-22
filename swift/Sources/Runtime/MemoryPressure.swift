// MemoryPressure.swift — PR-043. Subscribes to OS memory-pressure notifications
// (`DispatchSource.makeMemoryPressureSource`) and fans them out to registered
// purge handlers: bounded caches drop entries and stores release non-visible
// retained state on `.warning` / `.critical`.
//
// Levels (mapped onto the app's two-stage reclaim policy):
//   • .warning  → SHRINK: caches drop their LRU tail, backgrounded stores drop
//                 their heavy materialized views (keeping light summaries). The
//                 hot/foreground working set survives.
//   • .critical → PURGE: caches empty entirely; everything reclaimable goes.
//
// The monitor is deliberately a thin, injectable wrapper:
//   • Real OS source in `start()` (used by the app).
//   • `simulate(_:)` to drive a level synchronously in tests, with NO OS source,
//     so the purge wiring is verifiable deterministically.
//
// It holds only weak-ish closures (handlers the owner retains) so the monitor
// itself never keeps caches/stores alive.

import Foundation

/// Severity of a memory-pressure event, normalized away from the platform enum.
enum MemoryPressureLevel: Equatable {
    /// Reclaim cheaply: shrink caches, drop backgrounded heavy views.
    case warning
    /// Reclaim aggressively: purge caches entirely.
    case critical

    /// The shrink fraction a `BoundedCache` should target for this level.
    /// Warning keeps a fraction of the hot set; critical drops everything.
    var cacheRetentionFraction: Double {
        switch self {
        case .warning: return 0.25
        case .critical: return 0.0
        }
    }
}

/// Subscribes to memory-pressure events and notifies registered handlers. Each
/// handler is invoked on the main actor so it can touch `@MainActor` caches and
/// stores safely.
@MainActor
final class MemoryPressureMonitor {
    /// Registered reclaim handlers, invoked newest-registration-last on each event.
    private var handlers: [(MemoryPressureLevel) -> Void] = []
    private var source: DispatchSourceMemoryPressure?

    /// Count of events observed (real + simulated) — for tests / metrics.
    private(set) var eventCount = 0

    init() {}

    deinit {
        source?.cancel()
    }

    /// Register a reclaim handler. The returned token is opaque; handlers live for
    /// the monitor's lifetime (the app owns one monitor for the process).
    func addHandler(_ handler: @escaping (MemoryPressureLevel) -> Void) {
        handlers.append(handler)
    }

    /// Convenience: wire a `BoundedCache` so it shrinks on warning and purges on
    /// critical. Captures the cache strongly for the monitor's lifetime, matching
    /// the app where the cache and monitor are both process-lifetime singletons.
    func register<K, V>(cache: BoundedCache<K, V>) {
        addHandler { level in
            cache.purge(toFraction: level.cacheRetentionFraction)
        }
    }

    /// Start listening to the OS for real memory-pressure events. No-op if already
    /// started. The dispatch source delivers on a background queue; we hop to the
    /// main actor before invoking handlers.
    func start(queue: DispatchQueue = .global(qos: .utility)) {
        guard source == nil else { return }
        let src = DispatchSource.makeMemoryPressureSource(
            eventMask: [.warning, .critical],
            queue: queue
        )
        src.setEventHandler { [weak self, weak src] in
            guard let src else { return }
            let data = src.data
            let level: MemoryPressureLevel = data.contains(.critical) ? .critical : .warning
            Task { @MainActor in self?.dispatch(level) }
        }
        src.resume()
        source = src
    }

    /// Stop listening (e.g. on teardown). Safe to call repeatedly.
    func stop() {
        source?.cancel()
        source = nil
    }

    /// Synchronously drive a level to every handler. Used by tests and by any
    /// in-app heuristic that wants to force a reclaim. Deterministic — no OS source.
    func simulate(_ level: MemoryPressureLevel) {
        dispatch(level)
    }

    private func dispatch(_ level: MemoryPressureLevel) {
        eventCount += 1
        for handler in handlers {
            handler(level)
        }
    }
}
