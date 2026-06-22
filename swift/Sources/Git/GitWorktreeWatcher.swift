// GitWorktreeWatcher.swift — the real file-system watcher behind GIT-102.
//
// The Git studio's `debouncedRefresh()` coalesces a burst of refresh pokes into a
// single service round-trip, but until now NOTHING actually poked it from the file
// system — the "watcher" the store's comment referred to did not exist. This file
// supplies it:
//
//   * `GitWorktreeWatcher` — a tiny protocol (start(onChange:) / stop()) so the
//     store can be wired and TESTED with a fake, without spawning real FS events.
//   * `FSEventsWorktreeWatcher` — the real macOS implementation. An `FSEventStream`
//     watches the workspace tree and pokes `onChange` on its own dispatch queue;
//     the store hops to the main actor and calls `debouncedRefresh()`, so a storm
//     of file-system events still collapses to a bounded number of git reads (the
//     FSEvents latency coalesces, and the store's debounce coalesces again).
//
// The FSEvents delivery itself is a runtime behaviour (it needs real disk activity),
// so it is exercised in the running app; the store wiring + coalescing are unit
// tested through the protocol with a fake watcher.

import Foundation
import CoreServices

/// Watches a worktree and pokes `onChange` when its files change. Abstracted so the
/// store's wiring is testable with a fake (the real impl needs live disk events).
protocol GitWorktreeWatcher: AnyObject {
    /// Begin watching; `onChange` is invoked (possibly off the main thread) on each
    /// coalesced batch of file-system events.
    func start(onChange: @escaping () -> Void)
    /// Stop watching and release all resources. Safe to call more than once.
    func stop()
}

/// The real FSEvents-backed worktree watcher.
final class FSEventsWorktreeWatcher: GitWorktreeWatcher, @unchecked Sendable {
    private let path: String
    private let queue = DispatchQueue(label: "opensks.git.worktree-watcher")
    /// FSEvents-level coalescing latency (seconds): the OS already batches a burst
    /// before calling back; the store debounces again on top.
    private let latency: CFTimeInterval = 0.3

    private var stream: FSEventStreamRef?
    private var onChange: (() -> Void)?

    init(workspace: URL) {
        self.path = workspace.path
    }

    deinit {
        stop()
    }

    func start(onChange: @escaping () -> Void) {
        stop()
        self.onChange = onChange

        var context = FSEventStreamContext(
            version: 0,
            info: Unmanaged.passUnretained(self).toOpaque(),
            retain: nil,
            release: nil,
            copyDescription: nil
        )

        // @convention(c): the callback captures NOTHING; it recovers `self` from the
        // context `info` pointer and forwards to the stored closure.
        let callback: FSEventStreamCallback = { _, info, _, _, _, _ in
            guard let info else { return }
            let watcher = Unmanaged<FSEventsWorktreeWatcher>.fromOpaque(info).takeUnretainedValue()
            watcher.onChange?()
        }

        let flags = UInt32(
            kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagNoDefer
        )
        guard let stream = FSEventStreamCreate(
            kCFAllocatorDefault,
            callback,
            &context,
            [path] as CFArray,
            FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
            latency,
            flags
        ) else {
            self.onChange = nil
            return
        }

        FSEventStreamSetDispatchQueue(stream, queue)
        FSEventStreamStart(stream)
        self.stream = stream
    }

    func stop() {
        guard let stream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
        self.stream = nil
        self.onChange = nil
    }
}
