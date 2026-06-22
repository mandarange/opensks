//! Bounded caches with hard capacity guarantees (PR-043).
//!
//! [`BoundedLruCache`] is a generic least-recently-used cache that NEVER grows
//! past its configured capacity: inserting at the cap evicts the least-recently
//! used entry first. [`BoundedPageWindow`] is a FIFO window that caps the number
//! of retained pages, dropping the oldest page when a new one is pushed at the
//! cap. Both expose a `peak_len` high-water mark so a stress harness can prove
//! retained memory stayed within budget regardless of input size.
//!
//! The LRU is a safe, index-based intrusive doubly linked list over a slab
//! (`Vec<Node>`) with a free list — no `unsafe`, O(1) get/insert, deterministic
//! eviction order. Node values are stored as `Option<V>` so an evicted value
//! can be moved out with `Option::take` (no placeholder cloning).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::hash::Hash;

/// Sentinel index meaning "no node" in the intrusive linked list.
const NIL: usize = usize::MAX;

struct Node<K, V> {
    key: K,
    value: Option<V>,
    prev: usize,
    next: usize,
}

/// A least-recently-used cache with a hard capacity.
///
/// Invariant: `len() <= capacity()` always holds. When a fresh key is inserted
/// while the cache is full, the least-recently-used entry is evicted first, so
/// the cache can process an unbounded input stream with bounded memory.
pub struct BoundedLruCache<K, V> {
    capacity: usize,
    slab: Vec<Node<K, V>>,
    free: Vec<usize>,
    index: HashMap<K, usize>,
    head: usize,
    tail: usize,
    peak_len: usize,
    evictions: u64,
}

impl<K, V> BoundedLruCache<K, V>
where
    K: Eq + Hash + Clone,
{
    /// Create a cache that retains at most `capacity` entries. A capacity of
    /// zero is clamped to one so the cache always holds the most recent insert
    /// and the cap is a positive, meaningful bound.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            slab: Vec::with_capacity(capacity),
            free: Vec::new(),
            index: HashMap::with_capacity(capacity),
            head: NIL,
            tail: NIL,
            peak_len: 0,
            evictions: 0,
        }
    }

    /// The hard capacity. `len()` can never exceed this.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of live entries currently retained.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    /// Whether the cache holds no entries.
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// High-water mark of `len()` observed over the cache's lifetime. By the
    /// capacity invariant this never exceeds `capacity()`.
    pub fn peak_len(&self) -> usize {
        self.peak_len
    }

    /// Total entries evicted by the LRU policy over the cache's lifetime.
    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    /// Insert `key`/`value`, marking the key most-recently-used. If the key is
    /// new and the cache is at capacity, the least-recently-used entry is
    /// evicted and returned as `Some((evicted_key, evicted_value))`.
    pub fn insert(&mut self, key: K, value: V) -> Option<(K, V)> {
        if let Some(&node) = self.index.get(&key) {
            self.slab[node].value = Some(value);
            self.move_to_front(node);
            return None;
        }

        let mut evicted = None;
        if self.index.len() >= self.capacity {
            evicted = self.evict_lru();
        }

        let node = self.alloc_node(key.clone(), value);
        self.index.insert(key, node);
        self.push_front(node);
        self.peak_len = self.peak_len.max(self.index.len());
        evicted
    }

    /// Fetch a reference to the value for `key`, marking it most-recently-used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        let node = *self.index.get(key)?;
        self.move_to_front(node);
        self.slab[node].value.as_ref()
    }

    /// Fetch without changing recency order (a peek).
    pub fn peek(&self, key: &K) -> Option<&V> {
        let node = *self.index.get(key)?;
        self.slab[node].value.as_ref()
    }

    /// Whether `key` is currently retained (does not change recency).
    pub fn contains(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }

    fn alloc_node(&mut self, key: K, value: V) -> usize {
        let node = Node {
            key,
            value: Some(value),
            prev: NIL,
            next: NIL,
        };
        if let Some(slot) = self.free.pop() {
            self.slab[slot] = node;
            slot
        } else {
            self.slab.push(node);
            self.slab.len() - 1
        }
    }

    fn evict_lru(&mut self) -> Option<(K, V)> {
        let node = self.tail;
        if node == NIL {
            return None;
        }
        self.unlink(node);
        let evicted_key = self.slab[node].key.clone();
        self.index.remove(&evicted_key);
        let value = self.slab[node].value.take();
        self.free.push(node);
        self.evictions += 1;
        value.map(|value| (evicted_key, value))
    }

    fn push_front(&mut self, node: usize) {
        self.slab[node].prev = NIL;
        self.slab[node].next = self.head;
        if self.head != NIL {
            self.slab[self.head].prev = node;
        }
        self.head = node;
        if self.tail == NIL {
            self.tail = node;
        }
    }

    fn unlink(&mut self, node: usize) {
        let prev = self.slab[node].prev;
        let next = self.slab[node].next;
        if prev != NIL {
            self.slab[prev].next = next;
        } else {
            self.head = next;
        }
        if next != NIL {
            self.slab[next].prev = prev;
        } else {
            self.tail = prev;
        }
        self.slab[node].prev = NIL;
        self.slab[node].next = NIL;
    }

    fn move_to_front(&mut self, node: usize) {
        if self.head == node {
            return;
        }
        self.unlink(node);
        self.push_front(node);
    }
}

/// A FIFO window that retains at most `max_pages` pages. Pushing a page while at
/// the cap drops (and returns) the oldest retained page. Used to bound the
/// number of materialized event/log pages held for the UI at any instant.
pub struct BoundedPageWindow<P> {
    max_pages: usize,
    pages: VecDeque<P>,
    peak_len: usize,
    dropped: u64,
}

impl<P> BoundedPageWindow<P> {
    /// Create a window retaining at most `max_pages` pages (clamped to >= 1).
    pub fn new(max_pages: usize) -> Self {
        Self {
            max_pages: max_pages.max(1),
            pages: VecDeque::new(),
            peak_len: 0,
            dropped: 0,
        }
    }

    /// Maximum retained pages.
    pub fn capacity(&self) -> usize {
        self.max_pages
    }

    /// Pages currently retained.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Whether the window holds no pages.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// High-water mark of retained pages; never exceeds `capacity()`.
    pub fn peak_len(&self) -> usize {
        self.peak_len
    }

    /// Total pages dropped from the front to honor the cap.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Push a new page at the back. If at capacity, the oldest page is dropped
    /// from the front and returned so the caller can account for it.
    pub fn push(&mut self, page: P) -> Option<P> {
        let evicted = if self.pages.len() >= self.max_pages {
            self.dropped += 1;
            self.pages.pop_front()
        } else {
            None
        };
        self.pages.push_back(page);
        self.peak_len = self.peak_len.max(self.pages.len());
        evicted
    }

    /// Iterate the retained pages oldest-first.
    pub fn iter(&self) -> impl Iterator<Item = &P> {
        self.pages.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_never_exceeds_cap_and_evicts_lru_under_100k_inserts() {
        // Capacity invariant + LRU eviction order under a high-rate insert
        // stream: 100k unique keys into a 256-slot cache must NEVER retain more
        // than 256 entries, and the entries retained at the end must be exactly
        // the 256 most-recently inserted keys (strict LRU eviction order).
        let cap = 256usize;
        let total = 100_000u64;
        let mut cache: BoundedLruCache<u64, u64> = BoundedLruCache::new(cap);
        for key in 0..total {
            cache.insert(key, key.wrapping_mul(7));
            // The hard cap is enforced on every single insert.
            assert!(cache.len() <= cap, "cache grew past cap at key {key}");
        }
        assert_eq!(cache.len(), cap);
        assert_eq!(cache.peak_len(), cap);
        assert_eq!(cache.capacity(), cap);
        // Exactly `total - cap` evictions happened (every fresh key past the cap
        // evicted exactly one LRU victim).
        assert_eq!(cache.evictions(), total - cap as u64);

        // The retained set is precisely the last `cap` keys; everything older
        // was evicted as least-recently-used.
        let oldest_retained = total - cap as u64;
        for key in oldest_retained..total {
            assert!(cache.contains(&key), "expected recent key {key} retained");
            assert_eq!(cache.peek(&key), Some(&key.wrapping_mul(7)));
        }
        for key in 0..oldest_retained {
            assert!(!cache.contains(&key), "stale key {key} should be evicted");
        }
    }

    #[test]
    fn lru_get_marks_recently_used_and_survives_eviction() {
        // Touching a key with `get` makes it most-recently-used, so it survives
        // when fresh keys push the window forward.
        let mut cache: BoundedLruCache<&str, u32> = BoundedLruCache::new(3);
        cache.insert("a", 1);
        cache.insert("b", 2);
        cache.insert("c", 3);
        // Touch "a" so it is no longer the LRU victim.
        assert_eq!(cache.get(&"a"), Some(&1));
        // Inserting "d" must evict "b" (now the LRU), not the touched "a".
        let evicted = cache.insert("d", 4);
        assert_eq!(evicted, Some(("b", 2)));
        assert!(cache.contains(&"a"));
        assert!(!cache.contains(&"b"));
        assert!(cache.contains(&"c"));
        assert!(cache.contains(&"d"));
        assert_eq!(cache.len(), 3);
    }

    #[test]
    fn lru_reinsert_updates_value_without_growing() {
        let mut cache: BoundedLruCache<u8, u8> = BoundedLruCache::new(2);
        cache.insert(1, 10);
        cache.insert(1, 11);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.peek(&1), Some(&11));
    }

    #[test]
    fn lru_capacity_zero_clamps_to_one() {
        let mut cache: BoundedLruCache<u8, u8> = BoundedLruCache::new(0);
        assert_eq!(cache.capacity(), 1);
        cache.insert(1, 1);
        cache.insert(2, 2);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(&2));
        assert!(!cache.contains(&1));
    }

    #[test]
    fn page_window_caps_retained_pages() {
        let mut window: BoundedPageWindow<Vec<u8>> = BoundedPageWindow::new(4);
        for page in 0..1_000u32 {
            window.push(vec![page as u8; 16]);
            assert!(window.len() <= 4);
        }
        assert_eq!(window.len(), 4);
        assert_eq!(window.peak_len(), 4);
        assert_eq!(window.dropped(), 1_000 - 4);
    }
}
