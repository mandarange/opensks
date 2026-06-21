# Architecture Ownership Exceptions

The architecture guard (`scripts/check-architecture-ownership.sh`, wired into
`ci-core`) enforces the caps in `scripts/architecture-ownership.config`. See
[architecture-ownership.md](../architecture-ownership.md) for the full policy.

## When an exception is required

- **Lowering** a cap (recording monolith-reduction progress) — **no exception
  needed.** Edit the config down and commit.
- **Raising** a cap (e.g. `SRC_LIB_RS_MAX_LINES`), **adding a non-test module to
  `src/lib.rs`**, or otherwise relaxing a guard rule — **requires a reviewer-
  approved exception file in this directory, in the same pull request.**

## How to file an exception

1. Add a dated markdown file here named `YYYY-MM-DD-<short-slug>.md`.
2. Include, at minimum:
   - **What rule** is being relaxed and the **exact cap change** (old → new).
   - **Why** the change cannot land in a `crates/opensks-*` crate instead.
   - **Remediation plan / removal date** — every exception is temporary and must
     name the milestone or PR that retires it.
   - **Approver** — the reviewer who signed off.
3. Make the matching change to `scripts/architecture-ownership.config` in the
   same PR so the guard and the justification move together.

An exception that is not retired by its stated date is itself a review finding.
The intent of this directory is that raising a cap is always a visible, debated,
time-boxed decision — never silent drift back toward the monolith.
