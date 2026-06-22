// BoundedCache.swift — PR-043. A generic, capacity-bounded LRU cache for pages,
// thumbnails, and rendered items. Two hardening properties:
//
//   1. BOUNDED BY CONSTRUCTION. The cache holds at most `capacity` entries. The
//      (capacity + 1)-th insert evicts the least-recently-used entry FIRST, so
//      100,000 inserts into a 256-slot cache never exceed 256 live entries.
//      Memory is O(capacity), independent of insert count.
//
//   2. PURGEABLE ON MEMORY PRESSURE. `purge()` empties it; `purge(toFraction:)`
//      shrinks it to a fraction of capacity (dropping the LRU tail) so a
//      `MemoryPressure` warning can reclaim cache memory without discarding the
//      whole working set on every blip.
//
// Recency is tracked with an intrusive doubly-linked list over nodes plus a
// dictionary for O(1) lookup, so get/insert/evict are all O(1). This is a plain
// value-holding cache (no NSCache) so it is deterministic and unit-testable —
// eviction order is exact, not best-effort.

import Foundation

/// A capacity-bounded LRU cache. `Key` is hashable; `Value` is anything. Not
/// thread-safe by itself — callers confine it (e.g. `@MainActor` stores) or wrap
/// it. Eviction is exact LRU so tests can assert order.
final class BoundedCache<Key: Hashable, Value> {
    /// Hard upper bound on live entries. Always ≥ 1.
    let capacity: Int

    private final class Node {
        let key: Key
        var value: Value
        var prev: Node?
        var next: Node?
        init(key: Key, value: Value) {
            self.key = key
            self.value = value
        }
    }

    private var map: [Key: Node] = [:]
    // head = most-recently-used, tail = least-recently-used (the eviction victim).
    private var head: Node?
    private var tail: Node?

    /// Number of live entries. Always `<= capacity`.
    var count: Int { map.count }
    var isEmpty: Bool { map.isEmpty }

    init(capacity: Int) {
        self.capacity = max(1, capacity)
    }

    // MARK: - Access

    /// Look up a value, marking it most-recently-used on a hit.
    func value(forKey key: Key) -> Value? {
        guard let node = map[key] else { return nil }
        moveToHead(node)
        return node.value
    }

    subscript(key: Key) -> Value? {
        get { value(forKey: key) }
        set {
            if let newValue { insert(newValue, forKey: key) }
            else { removeValue(forKey: key) }
        }
    }

    /// Insert or update. On overflow, evicts the LRU entry FIRST so `count` never
    /// exceeds `capacity`. Updating an existing key refreshes its recency.
    @discardableResult
    func insert(_ value: Value, forKey key: Key) -> Value? {
        if let node = map[key] {
            node.value = value
            moveToHead(node)
            return nil
        }
        let node = Node(key: key, value: value)
        map[key] = node
        addToHead(node)
        // Evict from the tail until we are within capacity. The cap is enforced
        // BEFORE returning, so the cache is never observed over capacity.
        var evicted: Value?
        while map.count > capacity, let victim = tail {
            evicted = victim.value
            remove(victim)
        }
        return evicted
    }

    @discardableResult
    func removeValue(forKey key: Key) -> Value? {
        guard let node = map[key] else { return nil }
        remove(node)
        return node.value
    }

    func contains(_ key: Key) -> Bool { map[key] != nil }

    // MARK: - Memory pressure

    /// Drop EVERYTHING. Called on a critical memory-pressure event.
    func purge() {
        map.removeAll(keepingCapacity: false)
        head = nil
        tail = nil
    }

    /// Shrink to at most `fraction` of capacity by dropping the LRU tail. Called
    /// on a WARNING memory-pressure event to reclaim memory while keeping the hot
    /// working set. `fraction` is clamped to [0, 1]; 0 is equivalent to `purge`.
    func purge(toFraction fraction: Double) {
        let clamped = min(max(fraction, 0), 1)
        let target = Int((Double(capacity) * clamped).rounded(.down))
        if target <= 0 { purge(); return }
        while map.count > target, let victim = tail {
            remove(victim)
        }
    }

    // MARK: - Introspection (for tests)

    /// Keys ordered most-recently-used → least-recently-used.
    var keysByRecency: [Key] {
        var result: [Key] = []
        var node = head
        while let n = node {
            result.append(n.key)
            node = n.next
        }
        return result
    }

    // MARK: - Intrusive list ops (all O(1))

    private func addToHead(_ node: Node) {
        node.prev = nil
        node.next = head
        head?.prev = node
        head = node
        if tail == nil { tail = node }
    }

    private func moveToHead(_ node: Node) {
        guard head !== node else { return }
        unlink(node)
        addToHead(node)
    }

    private func remove(_ node: Node) {
        unlink(node)
        map[node.key] = nil
    }

    private func unlink(_ node: Node) {
        node.prev?.next = node.next
        node.next?.prev = node.prev
        if head === node { head = node.next }
        if tail === node { tail = node.prev }
        node.prev = nil
        node.next = nil
    }
}
