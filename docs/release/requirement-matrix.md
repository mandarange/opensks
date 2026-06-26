# OpenSKS Next — Release Requirement Matrix

The twelve product requirements of the conversation-first inversion, each mapped to
the PR(s) that deliver it and the **verifiable evidence** (tests, gates, schemas)
that proves it. This matrix is an evidence ledger, not a commercial-ready claim:
`Foundation` means contracts or partial verticals exist, `Partial` means a real
slice exists with known gaps, and `Blocking` means public commercial release must
not proceed until the named evidence exists.

| # | Requirement | PR(s) | Evidence | Status |
|---|-------------|-------|----------|--------|
| 1 | Project conversations are the primary unit of work | PR-022, PR-024, PR-025 | Conversation-first shell routing + project/conversation SQLite contracts + sidebar/thread persistence; NavigationTests; journey-without-CLI test | Partial |
| 2 | Runs & graphs are evidence-backed children of a conversation turn | PR-027, PR-029, PR-030 | v1 CLI turn links run projections; v2 daemon `conversation_turn_start` returns an accepted queued handle; accepted turns now snapshot persisted thread settings plus typed model routing decision JSON into the turn/run read model; queued turns have a durable TurnSupervisor claim/lease/heartbeat/recovery foundation; daemon `conversation_supervisor_tick` can claim one queued turn, execute explicit `simulation`-feature local-test paths or setup-required adapter paths, persist execution events, finalize run/message projections, and propagate terminal status into conversation summaries; repository migration repairs stale `running` summaries when no active run remains; no-default release builds fail structured local-test turns with `simulation_unavailable` instead of compiling or invoking `LocalTestAdapter`; OpenRouter tool-driver provider/protocol failures now terminate as failed run events instead of successful assistant finals; Swift Chat send now follows accepted handle with one typed supervisor tick and reloads messages/runs/summaries; always-on background supervisor loop, live Chat timeline subscription, and full provider/tool execution remain missing | Blocking |
| 3 | Code/Git/Design/Intelligence/Evidence are routed, labelled workspaces | PR-022, PR-041, PR-045 | `WorkspaceRoute` + `PrimaryWorkspaceRouter`; legacy route-section mapping remains | Partial |
| 4 | The daemon is an explicit streaming service | PR-026, PR-028 | NDJSON daemon emits explicit per-request `request_completed` terminal markers, Swift completes responses on that marker instead of quiet-window timing, and `SubscribeEvents` emits explicit v2 `stream_opened` / framed `event` / `stream_completed` or resumable `stream_failed` frames backed by `schemas/engine-stream-frame.schema.json`; full live provider/tool execution still gates end-to-end streaming smoke | Partial |
| 5 | The editor is a safe document system (not a colored preview) | PR-031, PR-032, PR-033 | Safe workspace file service exists; agent writes now route through `opensks-patch-engine` foundation with SHA-256 preimages, symlink containment, temp+fsync+rename apply, transactional delete/rename operations, typed rollback receipts, and fsynced redacted transaction journal events; agentic read/write/append tool calls pass through `ToolGateway` policy/path/output checks before read or patch planning | Partial |
| 6 | Git operations are typed, preconditioned, durable, approval-bound | PR-034, PR-035, PR-036 | Local Git mutation/outbox contracts exist; full release external-effect proof remains required | Partial |
| 7 | Design is a portable compiler/registry/audit engine | PR-037, PR-038, PR-039, PR-040 | Design package contracts/registry/compiler exist; hardcoded Swift seed cleanup remains | Partial |
| 8 | Dark-only identity, labelled nav, canonical logo, full hit areas, keyboard access, truthful states | PR-021, PR-023, PR-045 | UI tests exist; PR-077 screenshot/a11y commercial matrix is not attached | Partial |
| 9 | Memory & security stability are release evidence, not claims | PR-043, PR-044 | Perf/security foundations exist; capability truth now comes from the CLI runtime report with workspace/build fixture identity and runtime evidence overlay instead of a docs-only baseline; release runtime capability truth marks `agent.local_test_edit` unavailable when the `simulation` feature is disabled, and CI checks `cargo build --release --no-default-features` plus binary strings so developer-only `LocalTestAdapter` labels do not ship; CI now runs real `cargo deny`, `cargo audit`, gitleaks, CycloneDX SBOM generation, CodeQL Rust/Swift analysis, and uploads core/security/macOS evidence artifacts; `release proof` records required artifact SHA-256 digests, source commit SHA, tracked-worktree dirtiness, missing artifacts, blockers, and a same-SHA binding gate; sanitizer/E2E evidence plus signed/notarized/upgrade proof remain blockers | Blocking |
| 10 | Portable conversation summaries + encrypted vault | PR-042 | Redaction and `age` vault tests exist | Partial |
| 11 | Lifecycle / memory / high-rate performance hardening | PR-043 | Bounded cache/perf modules exist; crash-recovery scenarios remain unproven | Foundation |
| 12 | Security hardening + external audit gate | PR-044 | Workspace capability/path guards and dependency policy exist; agentic ToolGateway enforces ToolPolicy deny/read-only/ask, canonical workspace/symlink containment, allowed/forbidden path scope, binary/non-UTF8 stripping, output redaction, and size budget; runtime capability report records current ToolGateway/patch-engine/provider/protocol evidence including fsynced patch transaction journals, provider failure terminal semantics, and avoids stale quiet-window claims; separate executor registry/full approval bridge and release proof remain required | Blocking |

## Release readiness

- **Current release stance** — not public-commercial-ready. The no-model Chat path
  must fail setup-required, provider/tool/patch/event paths must be durable, and
  release proof must pass in clean CI with same-SHA artifact digests plus signed,
  notarized, upgrade, sanitizer, and E2E evidence.
- **Release simulation stance** — `LocalTestAdapter` is a developer/CI simulation
  fixture only. It is compiled for tests or the explicit `simulation` feature;
  no-default release builds must report `agent.local_test_edit` as unavailable and
  fail structured local-test turns without writing workspace files.
- **Required CI statuses** — `ci-core / core`, `ci-integration / integration`,
  `ci-security / security`, `ci-macos-app / macos-app`, CodeQL Rust/Swift,
  architecture, schemas, dependency audit, direct I/O audit, SBOM generation,
  secret scanning, and capability drift must exist on the candidate commit; the
  architecture guard must enforce line budgets, public function budgets, public
  function allowlists, and the Swift direct-process allowlist so domain services
  cannot instantiate `Process` outside the approved runtime/bootstrap owners.
  Missing statuses are release blockers.

## How to reproduce

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p opensks-adapter -p opensks-daemon -p opensks-cli --no-default-features -- -D warnings
cargo build --release --no-default-features
! strings target/release/opensks | grep -E 'Local test adapter|adapter:local-test|simulation lane'
cargo test --workspace --locked
cargo deny check
cargo audit
docker run --rm -v "$PWD:/repo" ghcr.io/gitleaks/gitleaks:v8.30.1 detect --source=/repo --config=/repo/.gitleaks.toml --no-git --redact
cargo install cargo-cyclonedx --version 0.5.9 --locked && cargo cyclonedx --format json --spec-version 1.5 --all-features
swift build  --package-path swift && swift test --package-path swift
swift-format lint --recursive swift/Sources swift/Tests
cargo run -p xtask -- schemas      # then: git diff --stat schemas  (must be clean)
cargo run -p xtask -- capability-matrix --runtime-fixture release
cargo run -p xtask -- architecture-graph --check
cargo run -p xtask -- direct-io-audit --check
scripts/check-architecture-ownership.sh
cargo run -- security audit        # 0 secret findings, 0 security findings
cargo run -- release proof         # must pass same-SHA artifact digest binding in clean CI
```
