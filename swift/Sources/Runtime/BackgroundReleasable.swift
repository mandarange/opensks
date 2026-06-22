// BackgroundReleasable.swift — PR-043. The contract a per-conversation / per-run
// store satisfies so that ONLY the foreground (active) one retains its full
// materialized view; backgrounded ones drop their heavy projection/rendered
// cache and keep only a light summary, then reload on re-activation.
//
// Why: a conversation/run store keyed by id can accumulate one fully-materialized
// view per id (a message page, a node projection, rendered items). Held forever,
// N background conversations retain N full views. The hardening rule: a single
// ACTIVE id keeps its heavy view; every other id is released to a light summary.
// Switching the active id releases the previous one and (re)hydrates the new one.
//
// "Light summary" means the cheap, list-level state that the sidebar still needs
// (title, counts, last-activity) — never the heavy page/projection bytes. The
// summaries list is unaffected; only the heavy per-id view is dropped.

import Foundation

/// A store that materializes a heavy per-id view and can release all but the
/// active id under backgrounding or memory pressure.
@MainActor
protocol BackgroundReleasable: AnyObject {
    /// Make `id` the foreground/active view: hydrate its heavy view and release
    /// every other id's heavy view down to a light summary. Passing `nil`
    /// backgrounds everything (no active heavy view retained).
    func setActive(_ id: String?) async

    /// Release the heavy materialized view for every id EXCEPT the active one,
    /// keeping light summaries. Idempotent. Driven by memory pressure or an
    /// app-level "background everything" transition.
    func releaseBackgroundViews()

    /// True if `id` currently retains its heavy materialized view (i.e. it is the
    /// active id and has been hydrated). For tests/assertions.
    func retainsHeavyView(_ id: String) -> Bool
}
