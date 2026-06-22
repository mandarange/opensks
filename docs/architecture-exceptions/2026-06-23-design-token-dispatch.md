# Architecture exception: design token subcommand dispatch (+3 lines, src/lib.rs)

**Date:** 2026-06-23
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20234 → 20237 (+3)
**Approved scope:** facade routing only — no domain logic in the monolith.

## Why

The recovery directive §16.3 (PR-056 / DESIGN-002 + DESIGN-101) requires the
Design Studio to persist edited token drafts, compile/validate them in isolation,
and enumerate the registry for a real (non-hard-coded) catalog. These surface as
three new `opensks design` subcommands — `save-tokens`, `compile`, `list` — which
the existing `run_design_command` facade routes to `run_design_studio_command`.

## What was added to src/lib.rs (+3 lines)

Three subcommand names added to the existing `matches!(sub, …)` routing guard in
`run_design_command` (the design-studio branch). No new dispatch arm, no wrapper,
no domain logic — the same facade that already routes `audit`/`activate`/
`revision-*`.

No domain logic lives in the monolith:
- token-draft persistence, isolated compile, and registry listing live in
  `crates/opensks-design/src/draft.rs` (`save_token_values`, `compile_package`,
  `list_packages`);
- the CLI subcommand bodies (stdin parsing, JSON contracts) live in
  `crates/opensks-cli` (`run_design_studio_command`).

## Follow-up

When the command facade is extracted from the monolith wholesale, this routing
moves with it and the cap can be lowered again (lowering needs no exception).
