# Architecture exception: `design` studio subcommand dispatch (PR-040)

**Date:** 2026-06-22
**PR:** PR-040 — Design Studio, Component Preview, and Audit
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20136 → 20152 (+16 lines)

## What

PR-040 adds audit / activate / active-status / revision-{propose,accept,reject,
rollback} to the `design` verb. Root `src/lib.rs` gains only a dispatch guard at
the top of `run_design_command` routing these subcommands to
`opensks_cli::run_design_studio_command`. All audit/activation/revision logic
lives in `crates/opensks-design` (audit.rs / activation.rs / revision.rs); the CLI
body lives in `crates/opensks-cli`. No new domain module is added to root.

## Why this is allowed

Identical pattern to the `file`, `conversation`, and `design import` dispatch:
root keeps only routing shims; the data-plane logic lives in dedicated crates.
