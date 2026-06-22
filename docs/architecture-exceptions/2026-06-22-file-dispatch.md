# Architecture exception: root `file` command dispatch (PR-032)

**Date:** 2026-06-22
**PR:** PR-032 — TextKit 2 Editable Code Workspace
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20116 → 20124 (+8 lines)

## What

PR-032 adds a top-level `file` CLI verb so the SwiftUI editor can open/save
workspace files through the hardened `opensks-file-service` (PR-031). Like every
other verb, the command body lives in `crates/opensks-cli`
(`opensks_cli::run_file_command`); the root `src/lib.rs` only gains the dispatch
arm plus a thin wrapper that forwards to the crate and maps the error:

- 1 line: `"file" => run_file_command(&args[1..], cwd),` in the dispatch match.
- 7 lines: the `run_file_command` wrapper (mirrors `run_conversation_command`).

## Why this is allowed

The data-plane / domain logic (containment, symlink/TOCTOU checks, atomic
replace, conflict detection, secret/binary/size guards) is entirely in
`opensks-file-service` + `opensks-cli`. Root keeps only the routing shim, exactly
as for `conversation`, `context`, `git`, etc. No new domain module is added to
`src/lib.rs`; the increase is the minimal wiring for one verb.
