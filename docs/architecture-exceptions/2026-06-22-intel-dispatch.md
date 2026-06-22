# Architecture exception: root `intel` command dispatch (PR-041)

**Date:** 2026-06-22
**PR:** PR-041 — Project Intelligence and Freshness UX
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20152 -> 20160

## What

PR-041 adds a top-level `intel` verb (freshness / freshness-check /
codegraph-query / glossary / architecture) so the app can browse project
intelligence with freshness indicators. The command body lives in
`crates/opensks-cli` (`opensks_cli::run_intel_command`), which forwards to the new
`opensks-intel` crate; root `src/lib.rs` only gains the dispatch arm plus a thin
wrapper (mirrors the `file` verb).

## Why this is allowed

All data-plane logic (freshness hashing, the stale-is-never-fresh check, paged
codegraph query, glossary/architecture readers) is in `opensks-intel` +
`opensks-cli`. Root keeps only the routing shim.
