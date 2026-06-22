# Architecture exception: root `security report` subcommand dispatch (PR-044)

**Date:** 2026-06-22
**PR:** PR-044 — Security Hardening and External Audit Gate
**Cap change:** `SRC_LIB_RS_MAX_LINES` -> 20186

## What

The `security` verb already exists in root `src/lib.rs` (the pre-existing
secret-leak `audit` gate). PR-044 adds a structured `opensks.security-report.v1`
aggregator whose body lives in `crates/opensks-cli`
(`opensks_cli::run_security_command`) over the `opensks-contracts` security schema.
Root gains only a small dispatch guard routing the NEW `report` subcommand to
opensks-cli; the existing `audit` gate is unchanged.

## Why this is allowed

All report-building logic (finding aggregation, severity rollup, blocking-finding
gate) lives in opensks-cli + opensks-contracts. Root keeps only the routing shim,
identical in spirit to the file/intel/vault/design subcommand dispatch.
