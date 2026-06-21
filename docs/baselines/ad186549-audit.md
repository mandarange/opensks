# Baseline Seal — `ad18654` Verified Audit

| Field | Value |
|---|---|
| Baseline commit | `ad18654935d351df6cff103f763eaa9b8983ff11` |
| Commit subject | `Build OpenSKS graph engine foundations` |
| Branch | `main` |
| Remote | `https://github.com/mandarange/opensks.git` |
| Audit date | 2026-06-21 (Asia/Seoul) |
| Method | Read-only multi-agent audit (8 subsystem readers + 10 adversarial verifiers), every claim backed by repo-relative `file:line` evidence. No files were modified during the audit. |

## Honesty preamble

This document records the **verified** state of the baseline. It deliberately
separates two facts the directive insists on never conflating:

1. Implementation files and CI workflow *definitions* exist in the repository.
2. A passing CI *result* for this commit does **not** exist. **"workflow exists"
   must never be read as "workflow passed."** See the CI section below — on this
   commit the `core` check actually **failed**.

Nothing here may be cited as "complete," "verified," or "passing" beyond what the
evidence states.

## Verified P0 truth map

Status legend: **confirmed** = directive claim matches code; **refuted** =
directive claim is wrong about the code; **partial** = true with an important
qualification.

| Claim | Status | Evidence (repo-relative) |
|---|---|---|
| UX-001 — rail selection does not route the central surface | confirmed | center keyed off `activeFileTab` (`swift/Sources/EditorView.swift:10`); `selectedRail` only switches the Explorer pane (`swift/Sources/ExplorerView.swift:22`); no `WorkspaceRoute`/`NavigationStack`/`NavigationSplitView` exists. Imperative reassignments of `selectedRail` after actions (e.g. `swift/Sources/Backend.swift:1194`) couple rail↔center via side effects, but selecting a rail item alone never re-renders the center. |
| UX-002 — editor is read-only, no save/undo/conflict/encoding | confirmed | self-described "virtualized read-only viewer" (`swift/Sources/EditorView.swift:2`); `FileTab` immutable `let lines` (`swift/Sources/Backend.swift:872`); no `TextEditor`/save/undo; encoding hardcoded UTF-8, breadcrumb cosmetically shows "UTF-8". |
| UX-003 — center letterbox | confirmed | `HomeView` clamps `maxWidth:720` then centers + 40pt padding (`swift/Sources/HomeView.swift:19`); per-line intrinsic-width `Text` + trailing `Spacer` inside a bidirectional ScrollView (`swift/Sources/EditorView.swift:84-95`); fixed rail 56 + bounded explorer/composer. |
| UX-004 — 56pt icon-only rail, labels only as tooltips | confirmed | `RailView().frame(width: 56)` (`swift/Sources/RootView.swift:39`); buttons are SF Symbols, English label only via `.help()` (`swift/Sources/RailView.swift:50-57`); on-screen label is in the Explorer header (`swift/Sources/ExplorerView.swift:12`). |
| UX-005 — composer is a right-side objective inspector | confirmed | pinned right panel (`swift/Sources/RootView.swift:63`); one `TextEditor` bound to `state.objective` (`swift/Sources/ComposerView.swift:56`); lanes list is read-only run telemetry, no message thread. |
| UX-006 — no project/conversation/message domain model | confirmed | grep for `Project`/`Conversation`/`Message`/`Chat`/`Thread` types → 0 hits; closest unit of work is a run + a single `objective` string (`swift/Sources/Backend.swift:906`). |
| God object — `AppState` owns everything | confirmed | one `@MainActor class AppState` (`swift/Sources/Backend.swift:882-915`) owns app data, engine connection, run control, terminal, rail route, file scan, tabs, objective, mode, palette, plus three sub-stores; it is the sole injected `environmentObject` (`swift/Sources/RootView.swift:28`). |
| Dual execution path on one `isRunning` | confirmed | `Start … run` → `startRun()` (CLI subprocess) and `Engine` → `startEngineRun()` (daemon) sit side by side (`swift/Sources/ComposerView.swift:101-122`); both guard/set the single `isRunning` (`swift/Sources/Backend.swift:982`, `:1329`). |
| RUN-001 — 150ms quiet-window completion, no terminal frame | confirmed | `let quietWindow: TimeInterval = 0.15` (`swift/Sources/Backend.swift:576`); completion = `sawRequestEvent && silence >= quietWindow` (`:580`); decoded `EngineEventType` has no terminal/done case (`swift/Sources/Models.swift:79-105`). Any inter-event gap > 150ms truncates a live stream. |
| RUN-002 — synchronous deterministic stub worker, no real LLM | confirmed | `DeterministicWorker::execute` returns canned `ok:true` (`crates/opensks-scheduler/src/lib.rs:174-191`); `dispatch_graph_run` runs `dispatch_until_idle` inline (`crates/opensks-engine/src/lib.rs:111-120`); no tokio/async/provider dep in engine or scheduler. |
| RUN-003 — pause/cancel/steer are write-only audit events | confirmed | `append_run_control_event` only appends one envelope, no transition/worker handle (`crates/opensks-engine/src/lib.rs:214-264`); scheduler has zero non-test handling of `RunPaused`/`RunCancelled`/`SteeringRequested`. The run already finished synchronously inside `run_start`, so there is nothing in-flight to control. |
| Stream — `SubscribeEvents` is a finite ≤5s polling tail | confirmed | `SUBSCRIPTION_TAIL_MAX_MS = 5_000` (`crates/opensks-daemon/src/lib.rs:21`); replay-since then bounded sleep-poll, single "tail completed" ordinary event (`:331-404`). No `stream_opened`/`heartbeat`/`completed`/`failed` frame taxonomy exists. |
| Identity — execution envelope lacks conversation binding | confirmed | `ExecutionEventEnvelope` (`crates/opensks-contracts/src/lib.rs:468-485`) carries `run_id`/`sequence`/`causation_id`/`correlation_id` only; `project_id`/`conversation_id`/`turn_id`/`message_id`/`node_id`/`graph_revision` are all absent (`graph.id` is improvised into `correlation_id`). |
| GIT-001 — no Git status/branch/stage/commit service | confirmed | only isolation/patch/worktree primitives (`crates/opensks-git/src/lib.rs:56-209`); only git subprocesses are rev-parse/worktree-add/lfs/apply/status; `enqueue_commit`/`enqueue_push` only build structs. |
| GIT-002 — in-memory outbox, no-op push executor | confirmed | `struct Outbox { items: Vec<OutboxItem> }` (`crates/opensks-git/src/lib.rs:212`), rebuilt per request (`crates/opensks-daemon/src/lib.rs:758`); executor closure is `\|_\| Ok(())` (`:795`). Approval-gating is real and tested but foundation-only since no push occurs. State does not survive restart. |
| Event store — WAL append/replay/redaction present; schema lacks new identities | confirmed | WAL + transactional append + ordered replay + value/key redaction (`crates/opensks-event-store/src/lib.rs:33,119-162,254-278`); schema is `runs`/`events`/`snapshots`/`evidence` keyed by `run_id`/`sequence` only — no conversation/turn/node/cursor/projection-version columns. |
| SEC-001 — no canonical-containment/TOCTOU/symlink safe-write service | confirmed | only write helper is `write_text_atomic` = tmp + `fs::rename`, no symlink/canonical/containment guard (`crates/opensks-artifacts/src/lib.rs:15-28`); ~215 raw `fs::write`/`File::create` sites in `src/lib.rs` with no containment handling. |
| Hooks — secret-before-hook block invariant present | confirmed | block-before-read returns `hook_secret_access_denied` before the hook acts (`crates/opensks-hooks/src/lib.rs:101-109`), tested and CI-gated. Detector is heuristic substring matching. |
| Retention — plan-only, no executor | confirmed | `plan_gc` classifies path strings and returns a plan; no `fs::remove_*` anywhere in the crate (`crates/opensks-retention/src/lib.rs:5-25`). |
| Monolith — `src/lib.rs` is a God module | confirmed | exactly **20,102 lines**; ~35 inline `run_*_command` handlers + domain types; only one module decl (`mod tests` at `src/lib.rs:15169`); zero internal-crate delegation. `src/main.rs` is a 57-line shim. |
| RES-001 — "logo SVG is not bundled" | **refuted (partial)** | The load-bearing sub-claim is false: `assets/opensks-logo.svg` **is** bundled. It is `include_str!`'d (`src/lib.rs:36`), written to `Contents/Resources/opensks-logo.svg` and rendered into `AppIcon.icns` as `CFBundleIconFile` by the Rust native bundler (`src/lib.rs:14785-14934`), with a test asserting its presence (`src/lib.rs:15907`). What is true: the **in-window** brand mark is the synthetic `AgentMark` (gradient + SF Symbol, `swift/Sources/Components.swift:24-40`), and `swift/Package.swift` declares no `resources:`, so SwiftUI cannot load the SVG at runtime. **Corrected gap:** the canonical SVG ships only as the app/Dock icon; wiring it into the in-window UI is the actual work — not "add a missing asset." |
| Status bars — "evidence-free status nouns" | **partial** | The StatusBar/TitleBar themselves are honest: proof counts + `HonestText.goalState` returns "Verifying"/"In progress"/"Unknown", never "Synced"/"Verified" (`swift/Sources/StatusBarView.swift:27-36`). Bare "Ready" leaks elsewhere (`swift/Sources/ComposerView.swift:48`; engine-daemon status, `swift/Sources/Backend.swift:964`). |
| MEM-001 — pending continuations hang on daemon death | **partial** | The bridge is a synchronous poll loop with no continuations, so the "hang until timeout" framing is technically refuted; the active request resolves promptly on `!session.isRunning` (`swift/Sources/Backend.swift:583`). Real risks are the 150ms premature-completion truncation and **unbounded** `partial`/per-response `lines` buffers (`swift/Sources/Backend.swift:113,218,273`). |

## Live / foundation / absent

- **Live (real local behavior):** CLI compatibility commands; SwiftUI shell (read-only viewer + fixed 4-column layout); persistent daemon child process + NDJSON bridge; `app-data` dashboard JSON path; logo SVG bundled as the macOS app icon.
- **Foundation (typed/tested scaffold, not real execution):** contracts/schemas, event store (WAL replay/redaction), scheduler (leases/governor — but not wired to real workers and not heartbeating in the dispatch path), engine run (deterministic stub worker), provider routing (fake/local), graph compile/templates, git isolation/patch + in-memory approval-gated outbox (no-op executor), hooks, codegraph, triwiki/context, image ledger, reasoning, policy, proof, retention (plan-only).
- **Absent at baseline:** project/conversation/message domain; editable code workspace; explicit stream framing (`stream_opened`/heartbeat/terminal); per-node live pipeline projection bound to a conversation; real async LLM worker + enforced pause/cancel/steer; Git status/branch/commit/push service + durable SQLite outbox + real push executor; safe-write file service (canonical containment/TOCTOU/symlink); portable design engine (package/registry/compiler/audit); the `.opensks/design-systems/` shared plane.

## CI and release-proof reality (firsthand-verified)

Verified directly via `gh` against `mandarange/opensks` at audit time for commit
`ad18654…`:

- **Combined Status API:** `state: "pending"`, `statuses: []`, `total_count: 0` —
  **no combined-status evidence is attached** to the baseline commit.
- **Checks API (4 runs):** `core: failure`, `integration: success`,
  `security: success`, `macos-app: success`. `ci-performance` is
  `schedule`/`workflow_dispatch`-only and **did not run** on this commit.
- **Why `core` failed:** the `core` job failed at the **Clippy** step
  (`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  exit 101); every later step in that job — including the architecture/path
  guard — was **skipped**. Locally, the same workspace passes Clippy clean
  (0 warnings) under `rustc 1.94.1` / `clippy 0.1.94`, so the CI failure is a
  **toolchain/lint-version drift on CI's `stable`**, pre-existing and
  independent of this baseline-seal work. It must be resolved (pin the toolchain
  or fix the new lint) before `ci-core` can serve as a required green check.

### Required vs present status checks

The directive names **11 required status checks**; the repository defines no
canonical enumeration of those 11 names and no branch-protection config. What is
verifiable in-repo: **5 workflow files exist** — `ci-core`/`core`,
`ci-integration`/`integration`, `ci-macos-app`/`macos-app`,
`ci-performance`/`performance`, `ci-security`/`security` — each a single job, no
matrix. Of these, `ci-performance` is schedule-only and cannot gate a PR,
leaving **4 PR-triggered status contexts**. The 6 missing required checks are
defined only by the external directive, not by the code.

## Corrections to the directive

1. **RES-001 is partly wrong.** The logo SVG *is* bundled (as the app icon). The
   real, narrower gap is that the in-window brand mark is synthetic and the SVG
   is unavailable to SwiftUI (no `Package.swift` resources). See the row above.
2. **CI is not merely "evidence-absent" — it is red.** `core` failed (Clippy) on
   the baseline; this is *stronger* than the directive's "no run evidence" claim.
3. **MEM-001 and BAR-001 are nuanced** (see the partial rows): no hung
   continuations; the two status bars are actually honest.

## Cross-references

- Architecture ownership + guard policy: [../architecture-ownership.md](../architecture-ownership.md)
- Runtime maturity by surface: [../runtime-truth-matrix.md](../runtime-truth-matrix.md)
- Guard implementation: `scripts/check-architecture-ownership.sh`
