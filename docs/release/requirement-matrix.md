# OpenSKS Next — Release Requirement Matrix

The twelve product requirements of the conversation-first inversion, each mapped to
the PR(s) that deliver it and the **verifiable evidence** (tests, gates, schemas)
that proves it. "Pass" means the cited evidence is green in the workspace gate
(`cargo clippy/test --workspace`, `swift build/test`, schema-drift, architecture
guard) and, where applicable, the `security audit` / `release proof` CLIs.

| # | Requirement | PR(s) | Evidence | Status |
|---|-------------|-------|----------|--------|
| 1 | Project conversations are the primary unit of work | PR-022, PR-024, PR-025 | Conversation-first shell routing + project/conversation SQLite contracts + sidebar/thread persistence; NavigationTests; journey-without-CLI test | Pass |
| 2 | Runs & graphs are evidence-backed children of a conversation turn | PR-027, PR-029, PR-030 | Conversation-turn→run vertical slice; node-level pipeline projection (reducer==rebuild); live pipeline UI + graph overlay | Pass |
| 3 | Code/Git/Design/Intelligence/Evidence are routed, labelled workspaces | PR-022, PR-041, PR-045 | `WorkspaceRoute` + `PrimaryWorkspaceRouter`; project intelligence workspace; every route reachable without CLI (ReleaseReadinessTests) | Pass |
| 4 | The daemon is an explicit streaming service | PR-026, PR-028 | Explicit streaming protocol v2 (framed, no quiet-window); scheduler command mailbox + real control semantics | Pass |
| 5 | The editor is a safe document system (not a colored preview) | PR-031, PR-032, PR-033 | Safe workspace file service (containment, symlink/TOCTOU, atomic); TextKit 2 editable code workspace; editor conflict + diff + incremental CodeGraph; file fuzz corpus (PR-044) | Pass |
| 6 | Git operations are typed, preconditioned, durable, approval-bound | PR-034, PR-035, PR-036 | Read-only git studio; local mutations (switch/stage/commit, no push); durable push outbox + approval-gated Commit & Push; `GitPush` requires approval (opensks-policy) | Pass |
| 7 | Design is a portable compiler/registry/audit engine | PR-037, PR-038, PR-039, PR-040 | Design package contracts + registry; deterministic compiler; human-reviewed import quarantine; Studio audits/atomic activation/revisions | Pass |
| 8 | Dark-only identity, labelled nav, canonical logo, full hit areas, keyboard access, truthful states | PR-021, PR-023, PR-045 | Dark semantic tokens + canonical logo; unified interaction components + accessibility; no-letterbox + hit-area + keyboard ReleaseReadinessTests; truthful empty/onboarding states | Pass |
| 9 | Memory & security stability are release evidence, not claims | PR-043, PR-044 | 100k-event stress within budget; supervisor zero-leak; `security audit` clean (0 findings) | Pass |
| 10 | Portable conversation summaries + encrypted vault | PR-042 | Redacted summary exporter (no raw transcript); `age` vault, fail-closed, wrong-key-no-leak (opensks-vault tests) | Pass |
| 11 | Lifecycle / memory / high-rate performance hardening | PR-043 | Bounded LRU caps under 100k inserts; event batcher bounded; background views released; `perf stress` within_budget | Pass |
| 12 | Security hardening + external audit gate | PR-044 | Workspace capability (deny-by-default, path-escape denied); ingress/persistence/export redaction proofs; approval/effect replay; deny.toml dependency posture | Pass |

## Release readiness

- **All user journeys complete without the CLI** — every `WorkspaceRoute` is
  reachable from `PrimaryWorkspaceRouter`; asserted by `ReleaseReadinessTests`
  (render + no-letterbox at 1024/1440, keyboard access, journey-without-CLI).
- **Proof package contains no secrets / private paths** — `opensks release proof`
  emits `opensks.release-proof.v1` with hashes/status/summaries only; verified to
  contain zero secret patterns and zero machine-absolute paths; the artifact lives
  under the git-ignored `.opensks/release/`.
- **Required CI statuses** — the workspace gate (cargo clippy/test, swift
  build/test, schema-drift, architecture-ownership guard) plus `security audit`
  (no open critical/high) are the statuses attached to the candidate commit.

## How to reproduce

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
swift build  --package-path swift && swift test --package-path swift
cargo run -p xtask -- schemas      # then: git diff --stat schemas  (must be clean)
scripts/check-architecture-ownership.sh
cargo run -- security audit        # 0 secret findings, 0 security findings
cargo run -- release proof         # opensks.release-proof.v1, no secrets/paths
```
