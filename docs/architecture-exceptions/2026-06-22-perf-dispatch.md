# Architecture exception: root `perf` command dispatch (PR-043)

**Date:** 2026-06-22
**PR:** PR-043 — Lifecycle, Memory, High-Rate Performance Hardening
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20168 -> 20177

## What

PR-043 adds a top-level `perf` verb (`perf stress`) so the runtime's bounded
event-batcher + LRU-cache stress harness can be run on demand and its
`opensks.perf-stress-report.v1` contract printed. The command body lives in
`crates/opensks-cli` (`opensks_cli::run_perf_command`), which forwards to the new
`opensks-perf` crate; root `src/lib.rs` only gains the dispatch arm plus a thin
wrapper (mirrors the `intel` / `file` verbs).

## Why this is allowed

All hardening logic — the process supervisor (deterministic reaping, zero
leaked handles, unambiguous cancellation ownership), the bounded LRU cache, the
bounded page window, the coalescing event batcher, and the 100k-event stress
harness — lives in `opensks-perf` (+ the thin `opensks-cli` wire-up). Root keeps
only the routing shim. The +9 lines are: one dispatch arm, one thin wrapper
function delegating to `opensks-cli`, and one `usage()` discoverability line; no
domain logic.
