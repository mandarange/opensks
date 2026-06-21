# OpenSKS Runtime Truth Matrix

Baseline sealed for the conversation-first directive (full verified audit:
[baselines/ad186549-audit.md](baselines/ad186549-audit.md)):

| Item | Value |
|---|---|
| OpenSKS baseline commit | `ad18654935d351df6cff103f763eaa9b8983ff11` (`Build OpenSKS graph engine foundations`) |
| Previous-sprint baseline | `9a96a61121147a9fff2c7340089ee2e681ab8fea` |
| CI evidence at baseline | No combined status (`total_count: 0`). Checks API: `core` = **failure** (Clippy step), `integration`/`security`/`macos-app` = success, `performance` did not run. "workflow exists" is **not** "workflow passed". |
| Work order SHA-256 | `abfc8d3d1cf323e3791b16cd966dae5f383477e04b614dfa6c4a9ce462d9f0fe` |
| Product language | English identifiers, schemas, logs, and UI copy |

## Current Maturity

| Surface | Current status | Trust note |
|---|---|---|
| Rust CLI compatibility commands | Local executable behavior | Existing `goal`, `qa`, `security`, and `provider list|probe|usage|adapter-check` commands remain compatibility surfaces. `opensks-cli` now owns daemon command parsing, daemon stdio handoff, `history init`, `graph templates|compile`, `hooks replay`, `codegraph index|query`, `triwiki seed`, `context pack`, `worktree create|isolate`, `provider route`, `patch propose|check`, `image ledger`, `reasoning debate`, `git outbox`, `gc plan`, `release proof`, `scheduler run|simulate|dispatch|recover`, and `worker runtime` as narrow `src/lib.rs` facade-extraction slices; most other command routing still lives in the root package. |
| SwiftUI app shell | Local native shell with CLI fallback | The app still supports `app-data` compatibility while the daemon bridge is introduced. `swift/Package.swift` is the app source of truth for CI and local bundle generation; root Rust no longer embeds `swift/Sources/*.swift` as `include_str!` payloads. |
| Data-plane manifest | P0 tracked/local boundary contract | `.opensks/data-plane-manifest.json` uses `opensks.data-plane-manifest.v1` to name shared durable paths such as wiki records, compact history, graph templates, architecture, and glossary records, and local/runtime-only paths such as runtime databases, caches, logs, secrets, worktrees, generated app bundles, raw history, and temporary image candidates. The manifest is a first-sprint bootstrap contract, not a GC executor. |
| Engine daemon | PR-004/006/009/014/017 run/replay/control/approval/outbox event-stream foundation | `opensks daemon --stdio --workspace <path>` emits typed `engine_hello`/`engine_health`, keeps stdin/stdout open, accepts each NDJSON request line as it arrives, routes pending stream requests through a bounded request worker set, and flushes response/event lines before stdin EOF. It accepts `run_start` for built-in templates or workspace-relative `graph_path`, `subscribe_events`, `run_pause`, `run_resume`, `run_cancel`, `run_steer`, `approval_request`, `approval_approve`, `approval_deny`, and `outbox_dispatch` requests. Runtime requests append/replay typed `ExecutionEventEnvelope` rows through the event store; `run_start` now drives deterministic local worker dispatch leases/results before snapshot writing; `subscribe_events` replays committed rows after a cursor and can bounded-tail poll for new rows during a finite request for reconnect/live-update foundation, without blocking later health/control requests on the same stdio session; outbox dispatch emits a typed dry-run `OutboxDispatchReport` that proves the dispatch callback is not called without matching approval. Swift now keeps a per-workspace long-lived daemon child process, sends multiple NDJSON requests through the same stdin/stdout session, decodes daemon/execution event labels as typed enums with unknown-label preservation, and routes pending responses by request/run ownership so concurrent and overlapping same-run request streams do not drain each other. This is still not a persistent live scheduler subscription, live provider worker bridge, or external side-effect executor. |
| Contracts and schemas | PR-008 foundation | `opensks-contracts` owns typed request/event/execution, model/provider, scheduler, graph, Git isolation, patch envelope, and completion proof DTOs with generated schemas under `schemas/`. |
| Event store | PR-003 foundation | `opensks-event-store` creates `.opensks/runtime/engine.sqlite3`, enables WAL, applies migration version 1, appends typed events before snapshots, replays ordered events with evidence refs, and redacts sensitive payloads. |
| Model/provider registry | PR-005 foundation | `opensks-provider` routes only enabled compatible fake/local model profiles, records routing decisions with snapshot hashes, blocks disabled models, capability mismatch, and unhealthy provider states. Live provider dispatch remains future work. |
| Durable scheduler / local worker runtime | PR-006 foundation | `opensks-scheduler` has typed work items, queue release by dependency, bounded governor decisions, event-before-state transitions through `opensks-event-store`, replay recovery helpers, a 10k simulated work-item test, deterministic local worker dispatch with provider-slot leases, worker outcomes, failure terminal handling, JSON dispatch reports, and lease heartbeat/expiry/recovery reports that requeue stale items after the lease-expired event append succeeds. `opensks worker runtime "<goal>"` writes local worker lease, heartbeat, daemon-visible bus, routing, and final-state artifacts under `.opensks/workers/<run-id>/`, including active leases, expired lease recovery by reassignment, and concurrent local request routing. Live provider/model/tool worker processes and a live remote provider worker bus are not yet dispatched. |
| Git isolation and patch transactions | PR-007 foundation | `opensks-git` detects Git repositories, creates real detached `git worktree` isolation when available, falls back to snapshot isolation for non-Git workspaces, guards dirty target paths, validates before hashes, and rolls back failed patch verification in fixture tests. Push/outbox UI is not live. |
| Graph compiler/templates | PR-008 foundation | `opensks-graph` defines default pipeline graph templates, validates loop bounds, side-effect approvals, terminal FinalSeal paths, compiles deterministic plan hashes, and writes `.opensks/pipelines/templates`. Visual graph editing is not live. |
| Queue/Runs/Steering UI | PR-009/bridge foundation | Swift `ExecutionStore` rebuilds run, queue, approval, and steering state from typed execution event envelopes, including running, cancelled, steering, and approval events. The Composer has Engine/Steer/approval actions and Runs has Pause/Resume/Cancel/Replay/Tail actions that send typed requests over the persistent daemon session and apply returned envelopes. The daemon has bounded pending request workers, and the Swift bridge has a bounded NDJSON line buffer, per-cli/per-workspace child-process reuse, typed event kind/severity/sensitivity decoding, and a pending response router that keeps request-correlated acks exact while assigning same-run execution envelopes to one active response owner. It is not yet a persistent live scheduler subscription bus or live worker event bridge. |
| Hook engine | PR-010 foundation | `opensks-hooks` dispatches deterministic ordered hooks, blocks secret payloads, handles timeout/block/modify/redirect/retry outcomes, and replays decision JSONL exactly. Hook inspector UI is not live. |
| Code Graph | PR-011 foundation | `opensks-codegraph` indexes Rust/Swift/TypeScript file/symbol/import/test records, supports one-file update, delete/rename cleanup, query, and local index artifacts. Full AST/call/reference/test ownership graph is not complete. |
| TriWiki/context/glossary/wrongness | PR-012 foundation | `opensks-triwiki` writes merge-friendly shared records under `.opensks/wiki/records`, blocks secret-looking shared writes, and `opensks-context` builds generated token-budgeted context packs. Full durable memory UI and clone bootstrap are not complete. |
| Project Intelligence UI | PR-013 foundation | Swift `ProjectIntelligenceStore` supports stale/fresh status, lazy visible-record windows, record counts, and click-to-source paths. Large graph LOD is tested; live large graph loading and full source navigation remain future work. |
| Graph Editor UI | PR-014 foundation | Swift `GraphEditorStore` supports nodes, edges, typed port diagnostics, undo/redo, visible-node LOD, Single Model Safe template load, graph draft save/load, contract `PipelineGraph` export, and daemon `graph_path` run over the persistent Swift bridge. Full canvas, inspector, palette, approval-aware side-effect editing, and live node overlays remain future work. |
| Image runtime | PR-015 foundation | `opensks-image` records image assets/anchors/temporary GC candidates, routes only enabled image-capable fake models, checks anchor bounds, and preserves before/after relations. Real provider image APIs and Swift inspectors remain future work. |
| Reasoning/debate | PR-016 foundation | `opensks-reasoning` emits bounded structured debate reports with evidence/counterexample fields and no hidden reasoning persistence. Live graph nodes/UI inspector remain future work. |
| Git Studio/outbox | PR-017 foundation | `opensks-git` has an outbox model that blocks secret-looking staged paths, keeps protected branch push in `awaiting_approval`, prevents duplicate remote writes by idempotency key, and proves a dry-run dispatch callback is not called without matching approval. The daemon can emit the same dry-run dispatch report. No live commit/push worker or external write executor consumes approvals yet. Swift Git Studio remains future work. |
| Retention/release hardening | PR-018 foundation | `opensks-retention` creates safe GC plans that protect active runs and shared records, plus release proof that stays NotVerified unless signing/notarization/fresh install/fresh clone/upgrade are all true. Real packaging/signing/notarization remain future work. |
| Full live engine completion | Roadmap | Live provider-backed workers, live remote provider worker bus, persistent background scheduler subscriptions, real approval leases/external side-effect enforcement, full graph editor, full AST CodeGraph, hook inspector, image providers, Git Studio, GC execution, and signed release packaging must not be claimed as complete. |

## Conversation-First Directive — Target Surfaces (absent at baseline)

These surfaces are required by the conversation-first directive and do **not**
exist at baseline `ad18654`. They must not be claimed as present. See
[baselines/ad186549-audit.md](baselines/ad186549-audit.md) for evidence.

| Target surface | Status | Note |
|---|---|---|
| Project / conversation / message domain | Absent | No `Project`/`Conversation`/`Message` type; planned owner `opensks-conversation`. |
| Editable code workspace | Absent | `EditorView` is a read-only viewer; no save/undo/conflict. |
| Explicit streaming protocol (terminal frames) | Absent | Completion inferred from a 150ms quiet window; `subscribe_events` is a ≤5s polling tail. |
| Live per-node pipeline projection bound to a conversation | Absent | Execution envelope lacks conversation/turn/node identity; run is synchronous. |
| Real async worker + enforced pause/cancel/steer | Absent | Deterministic stub worker; control events are write-only audit records. |
| Git Studio (status/branch/commit/push) + durable outbox | Absent | Only isolation/patch primitives; in-memory no-op outbox, no real push executor. |
| Safe-write file service (containment/TOCTOU/symlink) | Absent | Only an atomic-rename helper; ~215 raw write sites in `src/lib.rs`. |
| Portable design engine + `.opensks/design-systems/` plane | Absent | No design package/registry/compiler/audit; shared plane not created. |
| Canonical logo in the in-window UI | Absent | SVG ships only as the macOS app icon; the in-app mark is the synthetic `AgentMark`. |

## Migration Note

This sprint keeps the existing root `opensks` package as the compatibility
facade and adds workspace member crates for source-of-truth contracts, daemon,
event store, provider routing, scheduler, graph, Git isolation, policy, proof,
CLI routing, and engine planning. Existing commands should keep working while
new runtime code lands in crates instead of growing `src/lib.rs`.

## Rollback Note

Rollback is source-level: remove the new workspace members from `Cargo.toml`,
delete the new `crates/opensks-*` runtime foundation crates, `xtask`, generated
schemas, and `.opensks/pipelines/templates`, then restore the previous
`.gitignore` broad `.opensks/` rule only if shared wiki/project state should no
longer be trackable. No external writes, pushes, or publish steps are part of
this sprint.
