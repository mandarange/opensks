# Exception: raise `SRC_LIB_RS_MAX_LINES` 20108 → 20116

- **Date:** 2026-06-22
- **Rule relaxed:** `scripts/architecture-ownership.config` `SRC_LIB_RS_MAX_LINES`,
  raised from **20108** to **20116** (+8 lines).
- **Approver:** cdw0424 (repo owner).

## Why

PR-025 adds the `conversation` CLI verb. Its implementation lives entirely in
`crates/opensks-cli` (`run_conversation_command`, with tests). The root crate is
only the compatibility *dispatch facade*: the Swift app shells the root `opensks`
binary, so `src/lib.rs` must route `"conversation"` to the crate. That wiring is:

1. one match arm: `"conversation" => run_conversation_command(&args[1..], cwd),`
2. a 6-line thin wrapper that calls `opensks_cli::run_conversation_command` and
   maps the error — identical in shape to the existing `run_context_command`,
   `run_image_command`, etc. wrappers.

That is **+8 lines of facade routing, not domain logic**, which is precisely the
role `src/lib.rs` retains during migration (see
[../architecture-ownership.md](../architecture-ownership.md)). No new module is
added; the guard's "no non-test module" rule still holds.

## Removal / retirement

This allowance (and the prior facade growth) retires when CLI command routing
moves out of `src/lib.rs` into `opensks-cli`'s own dispatch entrypoint, per the
monolith-reduction milestones (P1 < 15,000 lines). At that point `src/lib.rs`
shrinks well below the cap and it should be lowered again.
