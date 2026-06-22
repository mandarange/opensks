# Architecture exception: `capability` command dispatch (+8 lines, src/lib.rs)

**Date:** 2026-06-23
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20226 → 20234 (+8)
**Approved scope:** facade dispatch only — no domain logic in the monolith.

## Why

The recovery directive §18.4 requires `opensks capability report --json`: a
machine-readable runtime capability report that CI, the app, and the generated
truth matrix all read from one honest source. Exposing it on the shipped product
binary requires a top-level command, which is routed through `run_cli` in
`src/lib.rs`.

## What was added to src/lib.rs (+8 lines)

1. One dispatch arm: `"capability" => run_capability_command(&args[1..], cwd),`.
2. A thin wrapper `run_capability_command` that delegates to
   `opensks_cli::run_capability_command` and maps the error type — identical in
   shape to the existing `perf`/`conversation` facade wrappers.

No domain logic lives in the monolith:
- the report value is built by `opensks_contracts::baseline_capability_report()`
  (crate `opensks-contracts`);
- the command body (`report` / `matrix` subcommands, JSON/markdown rendering)
  lives in `crates/opensks-cli`.

## Follow-up

When the conversation/command facade is extracted from the monolith wholesale,
this arm + wrapper move with it and the cap can be lowered again (lowering needs
no exception).
