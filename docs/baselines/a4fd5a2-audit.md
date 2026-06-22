# Baseline Audit — `a4fd5a2` (Chat-First Recovery Release)

> Baseline seal for the chat-first runtime/UI recovery directive.
> **Commit:** `a4fd5a262a9bce1c5b215c3fd008c779a56fe945`
> **Branch:** `main`
> **Method:** source-level verification (8 parallel readers, file:line evidence) cross-checked against the directive. Runtime/UI behaviour that needs a GUI, live model credentials, or remote CI is deferred to the PR-060 release gate and is **not** claimed here.

## 1. Environment baseline (measured)

| Check | Result |
|---|---|
| `cargo test --workspace` | **472 passed, 0 failed, 0 ignored** |
| `cargo clippy --workspace --all-targets` | **clean (exit 0)** |
| `swift build` (`OpenSKSStudio`, macOS 13+) | **Build complete (exit 0)** |

The previous baseline (`ad186549`) recorded a Clippy failure in `ci-core`; that is resolved at `a4fd5a2`.

## 2. Headline finding

The Chat → real-model → real-code-edit vertical path is **not** wired. A conversation turn runs a hard-coded `single-model-safe` graph dispatched by a `DeterministicWorker` that returns a fixed success stub; the assistant message is a templated run-summary string, and the runs list reports every run as `completed` regardless of real state. This is a deterministic *simulation* presented through the conversation UI, which is the root cause of "chat doesn't work / code isn't edited / SKS features don't run".

The safe-file, conversation-persistence, and reviewed-Git paths, by contrast, are real.

## 3. Verified defect registry

Status legend: ✅ confirmed · ◐ partially confirmed · ✱ refuted/corrected.

### P0 — runtime (Rust)

| ID | Status | Evidence (file:line) |
|---|---|---|
| CHAT-001 hard-coded graph + DeterministicWorker | ✅ | `crates/opensks-cli/src/lib.rs:2451-2454` (`run_template_with_event_stream(.., "single-model-safe", ..)`); `crates/opensks-engine/src/lib.rs:126-127` |
| CHAT-002 fixed assistant summary | ✅ | `crates/opensks-cli/src/lib.rs:2469`; `crates/opensks-scheduler/src/lib.rs:235` |
| CHAT-003 runs list hard-codes `completed` | ✅ | `crates/opensks-cli/src/lib.rs:2374` |
| RUN-001 DeterministicWorker success stub | ✅ | `crates/opensks-scheduler/src/lib.rs:225-235` |
| RUN-002 provider has routing, no executor | ✅ | `crates/opensks-provider/src/lib.rs:78` (`route`), `:218/:234/:275` (`fake_*` fixtures only) |
| STREAM-001 quiet-window completion | ✱ **corrected** | `opensks-stream` uses **explicit** terminal frames, no quiet window (`crates/opensks-stream/src/lib.rs:7,110,245`). The heuristic is **Swift-only** — see STREAM-002. |

### P0 — transport / shell (Swift)

| ID | Status | Evidence (file:line) |
|---|---|---|
| STREAM-002 v2 frame contract disconnected; quiet window in product path | ✅ | `swift/Sources/Backend.swift:190` (`EnginePendingResponseRouter`), `:576` (`quietWindow = 0.15`); v2 lives unused in `swift/Sources/EngineBridge/*` |
| SHELL-001 permanent right `ComposerView` | ✅ | `swift/Sources/RootView.swift:12,93,94` |
| SHELL-002 default route `.home` | ✅ | `swift/Sources/Navigation/NavigationStore.swift:9` |
| SHELL-003 dual nav state (`selectedRail` + route) | ✅ | `swift/Sources/Backend.swift:890`; `swift/Sources/Navigation/LabeledNavigationRail.swift:19`; `WorkspaceRoute.swift:56` |
| NAV-101 11 primary rail routes | ✅ | `swift/Sources/Navigation/WorkspaceRoute.swift:11` |
| NAV-102 route-independent legacy explorer | ✅ | `swift/Sources/ExplorerView.swift:38` (`switch state.selectedRail`) |
| LAYOUT-001 fixed-width columns | ✅ | `swift/Sources/RootView.swift:83,95`; `swift/Sources/Git/GitStatusView.swift:29,32` |
| PROC-101 per-call `Process` in 6 services | ✅ | ConversationService:175, FileService:183, GitService:250, DesignStudioService:141, IntelligenceService:119, VaultService:113 |
| PROC-102 sequential stdout/stderr drain | ✅ | same 6 services (`readDataToEndOfFile` back-to-back) |
| PROC-103 Task cancel ≠ child terminate | ✅ | only `Backend.swift:103` has `onTermination`; 6 services lack it |
| PROC-104 no `StudioTransport`/`ProcessSupervisor` | ✅ | `swift/Sources/Runtime/` has caches only |

### P0 — pipeline / editor / git / design / capability

| ID | Status | Evidence (file:line) |
|---|---|---|
| PIPE-001 no `pipelines.ingest` wiring | ✅ | `swift/Sources/Backend.swift:1044` applies to `executionStore` only; `PipelineProjectionStore` never fed |
| PIPE-002 reducer forbids running→paused | ✱ **corrected** | reducer uses strict `>` not `>=` (`PipelineProjectionStore.swift:331`); the real bug is the **rank model** itself (`opensks-contracts/src/projection.rs:49-71` ranks Paused<Running) — needs a transition model, not an operator flip |
| PIPE-003 no edges drawn | ✅ | `swift/Sources/Graph/PipelineGraphView.swift:92-112`; `GraphLayout.swift:6` ("no explicit edge set") |
| GRAPH-101 scroll-wheel zoom claimed, not impl | ✅ | `PipelineGraphView.swift:12` vs `:243` (pinch only) |
| GRAPH-102 no viewport reset on run change | ✅ | `PipelineGraphView.swift:36,73`; `PipelineGraphWorkspace.swift:43` |
| A11Y-102 Canvas single a11y element | ✅ | `PipelineGraphView.swift:74` |
| EDIT-001 full re-highlight per keystroke | ✅ | `CodeEditorRepresentable.swift:138,148` |
| EDIT-002 diff child process per content hash | ✅ | `EditorWorkspaceView.swift:174`; `EditorWorkspaceStore.swift:207`; `FileService.swift:183` |
| EDIT-003 nested `Button` in tab | ✅ | `EditorWorkspaceView.swift:106` |
| EDIT-004 dirty-close returns false, no dialog | ✅ | `EditorWorkspaceStore.swift:165-177`; `Backend.swift:1429` |
| TEXTKIT mix (TK2 view + TK1 storage APIs) | ✅ | `CodeEditorRepresentable.swift:37` vs `:149-166` |
| GIT-101 four fixed columns | ✅ | `swift/Sources/Git/GitStatusView.swift:27` (256/340/flex/320) |
| GIT-102 commit/push cards in-memory, not durable | ◐ | `GitStudioStore.swift:397`; `AppCoordinator.swift:221` (callback-posted cards) |
| DESIGN-001 `bindDesignStudio` never called | ✅ | defined `AppCoordinator.swift:132`, omitted in `RootView.swift:31-50` bootstrap |
| DESIGN-002 token edits in-memory only | ✅ | `DesignStudioStore.swift:291`; no save/compile/apply |
| DESIGN-101 hard-coded single-package catalog | ✅ | `AppCoordinator.swift:88,176` (`seedDesignCatalog`, `setCatalog` never called) |
| CAP-001 no machine-readable capability registry | ✅ | only prose `docs/runtime-truth-matrix.md`; no `CapabilityMaturity` |
| CI-001 no branch protection / linked checks | ✅ | 5 workflows, 4 PR-gated; no branch-protection config in repo |
| UX-102 internal jargon in user copy | ◐ | `PrimaryWorkspaceRouter.swift:35` (`PR-029/PR-030`), `Models.swift:360,367` (`Naruto`); `foundation` not found in user copy |

### §20 contracts / schemas / DB

- **All 16** directive schemas absent (`conversation-turn-start-request`, `-accepted`, `-thread-settings`, `timeline-item`, `agent-adapter-descriptor`, `agent-event-envelope`, `worker-role`, `subcontract-packet`, `patch-proposal`, `patch-apply-result`, `verification-result`, `pipeline-topology-snapshot`, `runtime-capability`, `runtime-capability-report`, `tool-policy`, `process-diagnostic`). `patch-envelope`, `pipeline-graph`, `pipeline-execution-projection` exist and are distinct.
- Schema mechanism: `schemars` + `opensks_contracts::schema_jsons()` → `cargo run -p xtask -- schemas` writes `schemas/*.json`; CI gate `git diff --exit-code -- schemas` (`.github/workflows/ci-core.yml:27`).
- **All 4** directive DB tables absent (`conversation_settings`, `timeline_items`, `run_projections`, `stream_cursors`). Migration mechanism: idempotent `execute_batch` in `ConversationRepository::migrate()` (`crates/opensks-conversation/src/lib.rs`), `MIGRATION_VERSION`.

## 4. Feasibility buckets (what can be proven here)

- **A — in-session, cargo-verifiable:** §20 schemas + DB tables, `CapabilityMaturity` registry + generated truth matrix, honest run-state (CHAT-003), agent-adapter abstraction + a local test adapter that *really* edits a fixture, architecture-guard rules.
- **B — Swift source, compiles here, visual proof deferred:** chat-first shell, single nav source, adaptive layout, `StudioTransport`/`ProcessSupervisor`, remove Swift quiet window, pipeline ingest + edges + a11y, editor hardening, Git adaptive layout, design persistence, copy cleanup.
- **C — needs live LLM credentials:** real model dispatch (CHAT-002/RUN-002), real assistant output.
- **D — needs GUI/XCTest + visual-regression infra:** UI automation (§23.3), visual regression (§23.4), 8-hour soak (§23.5).
- **E — needs org admin:** GitHub branch protection / linked required checks (CI-001, PR-060).

Buckets C/D/E are the directive's own PR-060 release gate and must be executed independently on real macOS with credentials; they are not asserted complete by source-level work.
