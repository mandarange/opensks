# OpenSKS

OpenSKS is a Rust-native autonomous coding OS prototype moving toward the
OpenSKS Next graph engine. The current first-sprint foundation keeps the
existing proof-first CLI and SwiftUI shell while introducing a workspace layout,
typed contracts, generated schemas, a stdio daemon health bridge, and foundation
crates for provider routing, durable scheduling, graph compilation, Git
isolation, policy, proof, and event-sourced engine planning.

Baseline and maturity are tracked in
[`docs/runtime-truth-matrix.md`](docs/runtime-truth-matrix.md), and the current
root/CLI public API extraction ledger is tracked in
[`docs/public-api-surface.md`](docs/public-api-surface.md). The current baseline
commit is `9a96a61121147a9fff2c7340089ee2e681ab8fea`.
The tracked/local OpenSKS data-plane boundary is machine-readable at
[`.opensks/data-plane-manifest.json`](.opensks/data-plane-manifest.json) and is
typed by `opensks.data-plane-manifest.v1`.

## Usage

```bash
cargo run
cargo run -- goal "Implement a bounded goal loop with Voxel TriWiki"
cargo run -- goal "Implement MCP browser QA" --mode naruto --max-waves 2
cargo run -- goal status <mission-id>
cargo run -- mcp list
cargo run -- mcp add local-demo "stdio://local-demo"
cargo run -- mcp audit
cargo run -- mcp describe
cargo run -- mcp invoke opensks.repo.search "goal loop"
printf '%s' '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | cargo run -- mcp serve --once
cargo run -- browser "https://example.com smoke test"
cargo run -- computer-use "inspect isolated desktop state"
cargo run -- app-use "inspect Finder accessibility tree"
cargo run -- voxel index
cargo run -- voxel query "goal"
cargo run -- cache warm
cargo run -- qa run
cargo run -- design qa
cargo run -- security audit
cargo run -- bench
cargo run -- auth
cargo run -- provider list
cargo run -- provider probe
cargo run -- provider usage
cargo run -- provider adapter-check
cargo run -- provider route code
cargo run -- graph templates
cargo run -- terminal smoke
cargo run -- terminal suggest --input "git st" --cwd "$PWD"
cargo run -- terminal agent "explain the last failing cargo test and suggest next commands"
cargo run -- daemon --stdio --workspace "$PWD"
printf '%s\n' '{"schema":"opensks.engine-request.v1","id":"req-run","kind":"run_start","protocol_version":"opensks.contracts.v1","params":{"pipeline_id":"single-model-safe","objective":"Smoke daemon graph run","run_id":"run-readme-daemon"}}' \
  | cargo run -- daemon --stdio --workspace "$PWD"
printf '%s\n' '{"schema":"opensks.engine-request.v1","id":"req-run-graph-path","kind":"run_start","protocol_version":"opensks.contracts.v1","params":{"pipeline_id":"editor-export-smoke","graph_path":".opensks/pipelines/templates/single-model-safe.graph.json","objective":"Smoke daemon saved graph run","run_id":"run-readme-graph-path"}}' \
  | cargo run -- daemon --stdio --workspace "$PWD"
printf '%s\n' '{"schema":"opensks.engine-request.v1","id":"req-approval","kind":"approval_request","protocol_version":"opensks.contracts.v1","params":{"run_id":"run-readme-daemon","approval_id":"approval-readme","scope":"git_push","message":"Approval required","reason_code":"approval_required"}}' \
  | cargo run -- daemon --stdio --workspace "$PWD"
cargo run -p xtask -- schemas
cargo run -- updater plan
cargo run -- acceptance audit
cargo run -- app
cargo run -- history init
cargo run -- scheduler run "verify local runtime"
cargo run -- scheduler simulate 128
cargo run -- scheduler dispatch 128
cargo run -- scheduler recover 8
cargo run -- worker runtime "verify local worker leases"
cargo run -- worktree create "worker lane one"
cargo run -- worktree isolate "worker lane one"
cargo run -- patch propose "describe a safe patch"
cargo run -- patch check README.md
cargo run -- graph compile extreme-parallel
cargo run -- hooks replay
cargo run -- codegraph index
cargo run -- codegraph query ExecutionStore
cargo run -- triwiki seed
cargo run -- context pack 120
cargo run -- image ledger
cargo run -- reasoning debate
cargo run -- git outbox
cargo run -- gc plan
cargo run -- release proof
cargo run -- prd coverage
```

Running `opensks` with no arguments, including double-clicking the built
`target/debug/opensks` file on macOS, writes `.opensks/macos/OpenSKS.app` and
opens the native SwiftUI shell. The current app is a local studio shell with
file browsing, composer controls, CLI output, and honest proof status. It now
performs a first-sprint daemon health check through
`opensks daemon --stdio --workspace <path>` and decodes typed `engine_hello` /
`engine_health` events. The daemon binary keeps stdin/stdout open, accepts
each NDJSON request line as it arrives, routes pending stream requests through a
bounded request worker set, flushes response/event lines before stdin EOF, and
still emits health on empty EOF for compatibility. A long bounded-tail
`subscribe_events` request no longer blocks a later health/control request from
being processed on the same stdio session. The daemon
also accepts a typed `run_start` request that compiles a graph template or a
workspace-relative `graph_path`, dispatches deterministic local worker leases
through the event store, replays execution envelopes, and writes a scheduler
snapshot;
the terminal surface now exposes `opensks terminal smoke|start|suggest|agent|explain|history`
and writes terminal runtime artifacts under `.opensks/runtime/terminal/`. The
safe smoke path executes a bounded `printf`, `suggest` and `agent` return
deterministic command proposals, and daemon stdio can route raw terminal
session/suggestion/agent-turn request kinds while still ending each parseable
request with `request_completed`. Persistent PTY sessions and live
provider-backed terminal execution are not yet connected.
the Composer's Engine action sends that request and applies returned execution
envelopes to the Runs/Queue stores. The Swift app now owns a per-workspace
long-lived daemon child process, writes health/run/control/approval/subscribe
requests over the same stdin pipe, and collects correlated NDJSON responses with
a bounded line buffer. Swift decodes engine event type/severity, execution
event kind, and sensitivity as typed enums that preserve unknown future labels;
its pending response router now assigns same-run execution lines to one active
response owner instead of duplicating them across overlapping `run_start` /
`subscribe_events` requests. The daemon also accepts
`subscribe_events` with a run id and replay cursor so the app can rebuild
Runs/Queue state after reconnect from committed event-store rows. The app also sends `run_pause`,
`run_resume`, `run_cancel`, and `run_steer` requests that append control events
to the event store; they do not yet terminate live provider workers.
It also sends `approval_request`, `approval_approve`, and
`approval_deny` requests that record auditable approval state; those events do
not yet enforce real external side-effect execution.
`subscribe_events` can also bounded-tail poll for new rows during one finite
request, and the Runs view exposes that as Tail from the current sequence
cursor. `opensks app-data <workspace>` remains the read-only compatibility fallback
during migration. The Swift sources live under
`swift/Sources/`, with `swift/Package.swift` added as the source-of-truth package
manifest for CI and local bundle generation. Native app bundling now builds the
`OpenSKSStudio` SwiftPM product and copies that executable into `OpenSKS.app`
instead of embedding Swift source files in the Rust binary. Secrets are never read into the UI, and completion language is gated so the
UI never claims a goal is complete that acceptance has not verified.
`.opensks/app/dashboard.html` remains a generated data artifact, not the launched
UI. Use `opensks --help` for the CLI command list without launching the app.

## Runtime Truth Matrix

| Surface | Status |
|---|---|
| CLI compatibility commands | Local executable behavior |
| CLI crate extraction | `opensks-cli` owns daemon command parsing, daemon stdio handoff, `history init`, `graph templates|compile`, `hooks replay`, `codegraph index|query`, `triwiki seed`, `context pack`, `worktree create|isolate`, `provider route`, `patch propose|check`, `image ledger`, `reasoning debate`, `git outbox`, `gc plan`, `release proof`, `scheduler run|simulate|dispatch|recover`, and `worker runtime`; the root package still owns most compatibility commands |
| Workspace contracts | `opensks-contracts` owns typed request/event/execution/work-item DTOs |
| JSON schemas | Generated under `schemas/` with `cargo run -p xtask -- schemas`, including the data-plane manifest contract that records shared tracked paths and local/runtime-only paths |
| Engine daemon | PR-004/006/009/014/017 foundation; persistent line-streaming stdio binary, bounded pending request routing, hello/health, `run_start` template or `graph_path`, `subscribe_events` replay/bounded tail, finite run control, finite approval, and outbox dispatch report streams |
| AI Terminal | PTY-backed local smoke command, deterministic command suggestions, agent command proposals, redacted internal terminal history, MCP-like terminal descriptor artifact, and daemon raw-request routing for terminal session/suggestion/agent-turn NDJSON with explicit `request_completed`; persistent PTY registry, Swift terminal pane, and live provider-backed execution remain gated |
| Event store | SQLite foundation via `opensks history init`; append/replay with evidence refs and snapshot tests cover PR-003 basics |
| Model/provider registry | PR-005 foundation via `opensks-provider`; fake/local routing only, no live dispatch |
| Durable scheduler / worker runtime | PR-006 foundation via `opensks-scheduler`; event-before-state simulation, deterministic local worker dispatch leases/reports, lease heartbeat/expiry/recovery reports, local worker bus/routing artifacts, and replay helpers, no live provider worker pool or live remote provider bus |
| Git isolation/patch transactions | PR-007 foundation via `opensks-git`; real git worktree fixture, dirty guard, rollback fixture, no push/outbox UI |
| Graph compiler/templates | PR-008 foundation via `opensks-graph`; deterministic compile, diagnostics, default templates, no visual editor |
| Queue/Runs/Steering UI | PR-009/bridge foundation via Swift `ExecutionStore`; Composer Engine/Steer/approval and Runs Pause/Resume/Cancel/Replay/Tail actions apply typed daemon event envelopes over the persistent daemon child process; daemon-side pending request routing plus Swift pending response ownership separate concurrent request streams, including overlapping same-run start/subscribe requests, but persistent live scheduler subscription remains future work |
| Hook engine | PR-010 foundation via `opensks-hooks`; deterministic replay, secret block, timeout/outcome tests, no hook inspector UI |
| Code Graph | PR-011 foundation via `opensks-codegraph`; Rust/Swift/TypeScript file/symbol/import/test index, no full AST graph |
| TriWiki/context | PR-012 foundation via `opensks-triwiki`/`opensks-context`; shared records and generated context packs, no full memory UI |
| Project Intelligence UI | PR-013 foundation via Swift `ProjectIntelligenceStore`; freshness, LOD, click-to-source state, no full live graph UI |
| Graph Editor UI | PR-014 foundation via Swift `GraphEditorStore`; template load, save/load graph draft, PipelineGraph export, daemon `graph_path` run, typed-port diagnostics, undo/redo, no full canvas |
| Image runtime | PR-015 foundation via `opensks-image`; image ledger, anchors, before/after relation, fake provider fallback only |
| Reasoning/debate | PR-016 foundation via `opensks-reasoning`; bounded structured reports, no hidden reasoning persistence |
| Git Studio/outbox | PR-017 foundation via `opensks-git` outbox; secret stage block, protected push approval policy, daemon/CLI approval-gated dry-run dispatch, idempotency, no live push worker |
| Retention/release | PR-018 foundation via `opensks-retention`; safe GC plan and honest NotVerified release proof, no signed/notarized app |
| Swift bridge | Keeps a per-workspace daemon child process, decodes typed daemon health/event envelopes with unknown-label preservation, sends `run_start`, `subscribe_events`, run control, and approval requests over the same stdio session, routes pending responses by request/run ownership so concurrent and overlapping same-run request streams do not steal lines, applies typed execution event envelopes, and keeps `app-data` fallback; it is not yet a persistent live scheduler subscription or live worker bridge |
| Full live engine dispatch, external side-effect approval enforcement, release packaging, signed distribution | Roadmap, not complete |

The CLI writes runtime artifacts under:

```text
.opensks/missions/<mission-id>/
```

Each mission currently includes:

```text
goal-loop.json
goal-state.jsonl
automation-loop.json
progress-ledger.json
stop-policy.json
tool-plan.json
goal-kind-registry.json
voxel-triwiki.json
voxels.jsonl
final-seal.json
prd-coverage.json
```

The final seal remains route-level `partial`, but it now carries an
`artifact_mvp_final_seal_integrity` trust contract for the local artifact
envelope. `acceptance audit` now marks `prod-005` passed only after reading the
latest mission `final-seal.json` and confirming that scoped trust contract;
without that evidence it remains partial. Live H-proof route completion,
provider-backed workers, repair waves, and final apply still remain false. The
current implementation proves intake, artifact
writing, automation-loop planning, capability planning, Voxel TriWiki seeding,
and PRD coverage accounting. Goal runs also write local scheduler, QA/security,
worktree-isolation, and patch-gate artifacts. `qa run` executes local Rust
checks when a Cargo workspace is present and always runs the built-in secret
scan; it also writes `secret-leak-rate.json`, `secret-leak-gate.json`, and
`secret-leak-release-history.json` so the current workspace release scan has an
explicit local release-history zero-leak denominator. Live external production
telemetry remains false. `cache warm`
hashes local cache segments and writes `cache-hit-report.json` plus
`cache-layout-improvement.json`, comparing the current stable prefix with the
previous warm snapshot. The beta-004 "Voxel TriWiki improves cache layout"
slice is bound only to the local `.opensks/cache/cache-layout-improvement.json`
artifact with schema `opensks.cache-layout-improvement.v1`, scope
`voxel_triwiki_cache_layout`, and strategy
`stable_prefix_dynamic_suffix`. Its local gate requires
`layout_gate_passed=true`, `baseline_available=true`,
`voxel_triwiki_segment_present=true`, and
`local_warm_prefix_hit_percent >= target_hit_percent`; it also records
`provider_metrics_available=false` and `live_provider_cache_metrics=false`.
`prod-001` passes only for that local stable-prefix artifact evidence when the
local warm-prefix hit is at least 95%; provider/runtime cache-layout
improvement and provider cached-token counters remain unverified. The
cache-layout gate only passes for the Voxel TriWiki scope when
`.opensks/triwiki/voxels.jsonl` is present in the stable prefix.
The planned/implemented beta-005 "Token dashboard tracks provider cache hit"
pass is also scoped to local artifact evidence only:
`cache-hit-report.json`, `cache-dashboard.json`, and
`providers/usage-dashboard.json` track provider cache-hit fields, source/status,
and estimated cached tokens. Live provider cached-token metrics remain
unavailable/not connected, so these dashboard fields must not be read as live
provider telemetry.
`bench` records timed local runtime checks plus scoped local/native
multi-session collaboration evidence artifacts: it can write
`native-collaboration-execution.json` and
`native-collaboration-events.jsonl` from `.sneakoscope` native agent session
evidence. This does not satisfy `beta-006` without independently verifiable
native-session provenance, and it does not claim live remote multi-provider API
worker collaboration, provider credentials, hidden fallback, or final apply. `auth` discovers configured provider environment variables without
exposing values and writes auth policy plus audit artifacts for Keychain-first
storage posture, OAuth candidates, API keys, and local endpoints. `provider list|probe|usage` writes provider
profiles, first-class/optional adapter capabilities, local endpoint
reachability probes, and zero-leak usage counters. `provider adapter-check`
writes OpenRouter/OpenAI adapter smoke evidence; authenticated remote checks are
only attempted when credentials are configured and
`OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1` is set.
`provider route code|text|image` uses deterministic fake/local model profiles
to write a typed `routing-decision.json` and prove the disabled-model,
capability-mismatch, provider-health, and single-compatible-model routing
invariants without making live remote provider calls.
Run `provider adapter-check` before rerunning `bench` when the collaboration
preflight should include adapter-check presence evidence.
`updater plan` writes the local `prod-006` signed-updates artifact set under
`.opensks/updater`: `update-manifest.json`, `update-signature.json`,
`update-channels.json`, `rollback-plan.json`, `update-boundary.json`, and
`updater-final-state.json`. The scoped pass is local manifest-signature
evidence only: the manifest requires signature and rollback, the signature
`manifest_hash` matches the manifest hash, the signature equals
`local_update_signature(manifest_hash)`, final state has
`signature_verified=true` and `network_or_install_performed=false`,
stable/latest channels require signature and rollback, the boundary requires
operator approval plus signature and rollback, and rollback apply transaction
is not live.
`prd coverage` writes the PRD coverage ledger plus
`requirement-coverage-gate.json`, which checks implemented plus artifact-MVP
requirement coverage against the 95% production threshold without claiming all
live acceptance criteria passed.
`acceptance audit` writes MVP/Beta/Production acceptance ledgers and findings so
remaining live gaps are explicit rather than inferred from green tests.
`voxel index` scans workspace text into code, symbol, design, security,
provider, package, and context voxels with stable/dynamic cache classification.
`browser` runs a local policy broker around curl network/page probes for
HTTP(S) targets, extracting title/hash/link/form/meta evidence while blocking
or approval-gating sensitive browser actions. The scoped `mvp-007` pass is
local deterministic browser runtime artifact evidence only: it writes
`browser-runtime/index.html`, `browser-interaction-loop.json`,
`browser-interaction-events.jsonl`, `browser-session.json` /
`session-summary.json` bindings, and local PPM screenshot artifacts for
open/screenshot/click/type evidence produced by `browser`. It does not claim
live Playwright/Chrome Extension/browser control, live DOM interaction, real
browser-rendered screenshots, external web control, or credential entry.
`computer-use` runs a local policy
broker: safe observation can attempt screenshot capture, while mouse/keyboard
and sensitive actions are blocked or marked approval-only and recorded in
action-plan/policy-decision artifacts. It also writes an isolated
browser/container observation-loop artifact set without launching live browser
control. The scoped `beta-002` pass is local isolated computer/browser
observation-loop evidence only: it requires `isolated-browser-container.json`,
`computer-browser-loop.json`, `computer-browser-loop-events.jsonl`,
`isolated-browser-runtime/index.html`, policy evidence, and final-state
evidence. Deterministic synthetic local HTML open/click/type event artifacts
are recorded with policy approval boundaries. This does not claim live browser
container control, live mouse/keyboard execution, external web control, or
arbitrary browser automation. `app-use` runs the same kind of broker
for native app intents. The scoped `mvp-008` pass, "App use can inspect macOS
accessibility tree", is local artifact evidence only: it writes
`accessibility-tree.json`, `running-apps.json`, `app-final-state.json`, and
policy/action-plan artifacts. Its acceptance gate requires a captured
accessibility-tree node or frontmost app, a running-app inventory, an attempted
inspection, a policy decision that allowed inspection, and
`live_app_actions_executed=false`. It does not claim full native app action
execution, arbitrary UI control, or live macOS app automation.
`app` writes a static local mission-control dashboard under `.opensks/app`
that summarizes PRD coverage, QA/security status, Voxel TriWiki counts,
provider configuration, missions, mission status, worker lanes, and
browser/computer/app-use sessions. It also writes `worker-lanes.json` plus
platform, module, macOS integration, source-note, and product statement
manifests that preserve PRD product posture without claiming live native GUI or
updater completion.
`scheduler run` writes the bounded stage plan, event stream, final state, and a
local `stage-overlap-report.json` from concurrent runtime metadata checks.
`scheduler simulate [count]` exercises the new durable scheduler foundation:
typed `SchedulerWorkItem`s are admitted through a bounded governor, state
transitions are appended to the event store before snapshot mutation, dependency
release is deterministic, and a local snapshot artifact records max concurrent
workers.
`scheduler dispatch [count]` exercises the local worker dispatch foundation:
ready work items acquire provider-slot leases, transition through
leased/dispatched/running/result-received/verifying/applying/completed states,
append every transition before state mutation, and write
`worker-dispatch-snapshot.json` plus `worker-dispatch-report.json`. This is
still deterministic local worker execution, not live external model/tool worker
process dispatch.
`scheduler recover [count]` exercises the lease recovery foundation: it leases
ready work items, records a `lease_heartbeat` event for one holder, expires stale
leases through `lease_expired` events, requeues stale items only after the event
append succeeds, and writes `lease-heartbeat-report.json`,
`lease-recovery-report.json`, and `lease-recovery-snapshot.json`. This proves
local heartbeat/TTL/requeue semantics; it is still not a daemon-visible live
worker bus or provider-backed worker process.
`worker runtime "<goal>"` writes a local daemon-visible worker bus artifact set
under `.opensks/workers/<run-id>/`: `worker-leases.json`,
`worker-heartbeats.jsonl`, `worker-bus.json`, `worker-routing.json`, and
`worker-final-state.json`. It records active leases, an expired lease recovered
by reassignment, heartbeat expiry metadata, and concurrent local request
routing. This is still a local artifact runtime: live provider workers and a
live remote provider bus remain explicitly false.
`prod-002` is scoped to that local scheduler overlap target: `acceptance audit`
binds it to `.opensks/scheduler/*/stage-overlap-report.json` and can mark it
passed only when `target_met=true`, `observed_parallel_execution=true`,
`overlap_ratio>=target_ratio`, and every recorded stage span passed. This is
not provider/production worker tuning; production worker overlap tuning remains
partial and is still a gap.
`worktree create` remains the legacy compatibility snapshot command under
`.opensks/worktrees/<id>/workspace`. `worktree isolate` creates a real detached
`git worktree` under `.opensks/runtime/worktrees/<run>/<worker>/` when the
workspace is a Git repo and falls back to snapshot isolation for non-Git
workspaces. `patch check` writes a typed patch envelope and dirty-path guard
artifact; actual patch apply is covered by crate fixture tests and still
requires later Outbox/approval UI.
`graph templates` writes default graph templates under
`.opensks/pipelines/templates`, and `graph compile [template-id]` writes a
deterministic compiled plan with diagnostics for loop bounds, side-effect
approval, port compatibility, and FinalSeal terminal paths. The visual graph
editor now has a finite Swift foundation: the Graph rail can load the Single
Model Safe template, save/load the current graph draft at
`.opensks/pipelines/editor/current.graph-editor.json`, export a contract graph at
`.opensks/pipelines/editor/current.graph.json`, and run that saved graph through
the daemon `run_start` bridge with workspace-relative `graph_path`. Full canvas
editing, palette, inspector, approval-aware side-effect node editing, and live
node overlays remain future work.
`hooks replay` writes deterministic hook decisions and verifies exact replay.
The hook foundation covers ordering, timeout, secret block, and
block/modify/redirect/retry outcomes, but not a live hook inspector UI.
`codegraph index|query` writes a local CodeGraph index under ignored
`.opensks/wiki/indexes/` for Rust, Swift, and TypeScript source symbols. It is
a source-oriented local index, not the full AST/call/reference/test ownership
engine. `triwiki seed` writes merge-friendly shared records under
`.opensks/wiki/records`, and `context pack` writes generated token-budgeted
context packs under ignored `.opensks/wiki/context-packs/generated/`.
`image ledger` writes an ignored candidate image ledger and verifies enabled
image-model fallback plus anchor bounds. `reasoning debate` writes a bounded
structured debate report with evidence/counterexample fields and no hidden
reasoning persistence. `git outbox` writes an ignored outbox plan, keeps
protected push as `awaiting_approval`, proves the dispatch callback is not
called without matching approval, and records a dry-run approved dispatch without
performing a live remote write. It also models idempotency. `gc plan` writes a
safe retention plan that keeps active runs and shared records; `release proof`
stays `NotVerified` until signing, notarization, fresh install, fresh clone, and
upgrade proof are all present.
Non-goal computer/app capability commands still create the PRD-named
audit/session artifacts with explicit non-live status where the full engine does
not exist yet. `design qa` scans local design surfaces for static
accessibility, responsive, and color token findings. The scoped `beta-003`
pass is deterministic local raster screenshot artifact plus pixel diff evidence:
it writes local PPM screenshot artifacts, `design-screenshot-snapshots.jsonl`,
and `design-screenshot-diff-report.json` from the local renderer state between
runs. This does not claim live browser-rendered screenshot capture, Chrome
Extension evidence, gpt-image-2/ImageGen review, Product Design plugin visual
comparison, or external design service execution.
`security audit` scans workspace text for secrets,
prompt-injection-like phrases, MCP allowlist bypass phrasing, unsafe shell
actions, and supply-chain shell pipes, then writes the same
`secret-leak-rate.json` and `secret-leak-gate.json` gate artifacts under `.opensks/security`. `mcp describe`, `mcp invoke`, and
`mcp serve --once` expose a local brokered MCP-style JSON-RPC surface for
allowlisted OpenSKS tools such as workspace search, Voxel query, final-seal
reads, and local QA.

## PRD Coverage State

`cargo run -- prd coverage` writes `.opensks/prd-coverage.json` and
`.opensks/requirement-coverage-gate.json`. The gate measures artifact-backed
PRD requirement coverage (`implemented + artifact_mvp`) against the 95%
threshold; it is not a completion claim and does not mean live acceptance has
all passed.

Still not live:

- Live remote provider dispatch, OAuth/Keychain integration, and provider-backed worker execution
- External MCP client/server transports beyond the local stdio JSON-RPC
  one-shot surface
- Full Playwright browser control, screenshots, clicks, typing, and DOM capture
- Live browser-rendered screenshots, Chrome Extension evidence, gpt-image-2/
  ImageGen review, Product Design plugin visual comparison, and external design
  service execution
- Dynamic dependency vulnerability resolution and sandboxed exploit testing
- Desktop mouse/keyboard action execution beyond brokered policy decisions
- macOS accessibility/app automation beyond brokered inspection and inventory
- Full visual graph editor canvas/inspector/palette, full AST CodeGraph, hook inspector UI, live reasoning/debate graph nodes, real image providers, repair waves, and final apply transactions
- Signed/notarized app release, real GC execution, and production release packaging
- Fully concurrent Swift daemon event bus, independent Swift CI packaging, and release notarization beyond the current local shell
- Production crypto/notarized updater apply, network install/apply, and
  production-grade acceptance targets
