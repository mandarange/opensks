# Architecture Ownership Map

This document defines **who owns what** in the OpenSKS workspace and the
guardrail that keeps that ownership from eroding. It is the reference for the
`scripts/check-architecture-ownership.sh` CI guard.

Baseline: commit `ad18654935d351df6cff103f763eaa9b8983ff11` (branch `main`,
remote `https://github.com/mandarange/opensks.git`). See
[baselines/ad186549-audit.md](baselines/ad186549-audit.md) for the verified
state of every subsystem at this baseline.

## Source-of-truth principle

```
SwiftUI views  ->  Swift domain stores  ->  typed service protocols
              ->  versioned wire contracts (opensks-contracts + schemas/)
              ->  opensks-daemon (transport/dispatch only)
              ->  domain service crates (conversation, engine, scheduler, git,
                  design, file, policy, ...)
              ->  append-only evidence (event store, artifacts, proof)
```

Rules that the guard and review enforce:

- **Public wire schema is owned by Rust contracts + generated JSON Schema**
  (`crates/opensks-contracts` + `schemas/`), not by hand-edited Swift or docs.
- **Domain logic lives in `crates/opensks-*`, never in the root crate.**
  `src/lib.rs` is a *compatibility facade* during migration, not a home for new
  subsystems. `src/main.rs` is a thin entry shim.
- **The daemon is transport/dispatch**; it delegates to domain services and must
  not host domain implementations.
- **Views do not spawn processes, read/write files, or hand-format daemon JSON.**

## Ownership map

`Owner` is the crate (or module) that owns the implementation. `Status` reflects
the verified baseline: **live** (real behavior), **foundation** (typed/tested
scaffold, not yet wired to real execution), or **planned** (does not exist yet).

| Domain | Owner | Status |
|---|---|---|
| Wire contracts / schemas | `opensks-contracts` (+ `schemas/`, `xtask`) | foundation |
| CLI command routing | `opensks-cli` (facade-extraction from `src/lib.rs`) | live (compatibility) |
| Daemon transport / dispatch | `opensks-daemon` | foundation |
| Run orchestration | `opensks-engine` | foundation (deterministic local worker) |
| Scheduling / leases / governor | `opensks-scheduler` | foundation (synchronous; not wired to real workers) |
| Event store / replay / redaction | `opensks-event-store` | foundation |
| Provider / model routing | `opensks-provider` | foundation (fake/local profiles) |
| Graph templates / compile | `opensks-graph` | foundation |
| Git isolation / patch / outbox | `opensks-git` | foundation (in-memory outbox, no-op executor) |
| Hook engine | `opensks-hooks` | foundation |
| Code graph index | `opensks-codegraph` | foundation |
| TriWiki / context / glossary | `opensks-triwiki`, `opensks-context` | foundation |
| Image asset ledger | `opensks-image` | foundation |
| Reasoning / debate | `opensks-reasoning` | foundation |
| Policy / permission decisions | `opensks-policy` | foundation |
| Completion proof | `opensks-proof` | foundation |
| Retention / GC planning | `opensks-retention` | foundation (plan-only, no executor) |
| Artifacts / redaction helpers | `opensks-artifacts` | foundation |
| Test fixtures | `opensks-testkit` | foundation |
| SwiftUI app shell | `swift/Sources/*` (single SPM target) | foundation (read-only viewer, fixed shell) |
| **Project / conversation / message** | `opensks-conversation` | **planned (does not exist)** |
| **Explicit stream framing** | `opensks-stream` | **planned (does not exist)** |
| **Safe workspace file service** | `opensks-file-service` | **planned (does not exist)** |
| **Git status/branch/commit/push service** | `opensks-git-service` | **planned (does not exist)** |
| **Portable design engine** | `opensks-design` | **planned (does not exist)** |

The four `planned` service crates may begin life as modules inside an existing
crate; what matters is **ownership and a test boundary**, not crate count. They
must not be implemented inside `src/lib.rs`.

Release/GC CLI command bodies live in `crates/opensks-cli/src/retention.rs` and
delegate trust decisions to `opensks-retention`, keeping `crates/opensks-cli/src/lib.rs`
under its public-function and line-count budget.

## Root monolith reduction policy

`src/lib.rs` was **20,102 lines** at baseline and still hosts ~35 inline
`run_*_command` handlers plus domain types. It is a known God module. The inline
test mega-module has been moved to `src/tests.rs`, and the static PRD coverage
ledger now lives in `assets/prd-requirements.tsv`, lowering the current root
facade cap to **14,870 lines**. The plan:

| Milestone | Target for `src/lib.rs` |
|---|---|
| P0 (now) | No new domain type or handler except re-export/facade. **Zero growth.** |
| P1 | < 15,000 lines (**met by current 14,870-line ratchet**) |
| P2 | < 8,000 lines |
| Final | Compatibility facade, ideally < 2,500 lines |

Lowering the cap to record progress is always welcome and needs no ceremony.

## The guard

`scripts/check-architecture-ownership.sh` (run from repo root; wired into the
`ci-core` workflow) enforces, with caps from
`scripts/architecture-ownership.config`:

1. `src/lib.rs` line count `<= SRC_LIB_RS_MAX_LINES` (current ratchet 14870,
   reductions only).
2. `src/main.rs` line count `<= MAIN_RS_MAX_LINES` (thin shim).
3. `src/lib.rs` declares **no non-test module** (a new subsystem must be a crate).
4. `.opensks/data-plane-manifest.json` exists (shared/local boundary contract).
5. `.gitignore` has **no broad `.opensks/` rule** (shared durable records —
   wiki, architecture, glossary, history summaries, and the future
   `.opensks/design-systems/` — must stay trackable; use specific
   `.opensks/<subdir>/` rules).
6. No developer-machine absolute paths or forbidden PRD source markers in
   tracked sources/docs.

Run it locally before pushing:

```bash
scripts/check-architecture-ownership.sh
```

To **raise** a cap or relax a rule, see
[architecture-exceptions/README.md](architecture-exceptions/README.md): it
requires a reviewer-approved, time-boxed justification file in the same PR.

## Note on the design-systems data plane

`.opensks/design-systems/` does not exist yet (it arrives with the design
engine, PR-037+). When it is added it MUST be a *shared durable* tracked plane.
The current `.gitignore` rule `.opensks/design/` (design QA artifacts) does not
cover `.opensks/design-systems/`, so no broad rule currently hides it — but the
guard's broad-`.opensks/` check exists to keep it that way.
