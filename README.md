# OpenSKS

OpenSKS is a Rust-native autonomous coding OS prototype. The current vertical
slice implements the PRD v3 `/goal` foundation: a proof-first goal-loop intake
that writes mission artifacts, stop policy, tool plan, Voxel TriWiki seed data,
progress ledger, and a final seal.

## Usage

```bash
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
cargo run -- updater plan
cargo run -- acceptance audit
cargo run -- app
cargo run -- scheduler run "verify local runtime"
cargo run -- worktree create "worker lane one"
cargo run -- patch propose "describe a safe patch"
cargo run -- prd coverage
```

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
`bench` records timed local runtime checks plus explicit multi-LLM roster,
role-assignment, disagreement, quorum, and collaboration preflight artifacts
with hidden fallback disabled. `auth` discovers configured provider environment variables without
exposing values and writes auth policy plus audit artifacts for Keychain-first
storage posture, OAuth candidates, API keys, and local endpoints. `provider list|probe|usage` writes provider
profiles, first-class/optional adapter capabilities, local endpoint
reachability probes, and zero-leak usage counters. `provider adapter-check`
writes OpenRouter/OpenAI adapter smoke evidence; authenticated remote checks are
only attempted when credentials are configured and
`OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1` is set.
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
or approval-gating sensitive browser actions. `computer-use` runs a local policy
broker: safe observation can attempt screenshot capture, while mouse/keyboard
and sensitive actions are blocked or marked approval-only and recorded in
action-plan/policy-decision artifacts. It also writes an isolated
browser/container observation-loop seed and ledger without launching live
browser control. `app-use` runs the same kind of broker
for native app intents: safe inspection captures frontmost and running-app
state, while sensitive native actions are blocked or approval-only.
`app` writes a static local mission-control dashboard under `.opensks/app`
that summarizes PRD coverage, QA/security status, Voxel TriWiki counts,
provider configuration, missions, mission status, worker lanes, and
browser/computer/app-use sessions. It also writes `worker-lanes.json` plus
platform, module, macOS integration, source-note, and product statement
manifests that preserve PRD product posture without claiming live native GUI or
updater completion.
`scheduler run` writes the bounded stage plan, event stream, final state, and a
local `stage-overlap-report.json` from concurrent runtime metadata checks.
`prod-002` is scoped to that local scheduler overlap target: `acceptance audit`
binds it to `.opensks/scheduler/*/stage-overlap-report.json` and can mark it
passed only when `target_met=true`, `observed_parallel_execution=true`,
`overlap_ratio>=target_ratio`, and every recorded stage span passed. This is
not provider/production worker tuning; production worker overlap tuning remains
partial and is still a gap.
Non-goal computer/app capability commands still create the PRD-named
audit/session artifacts with explicit non-live status where the full engine does
not exist yet. `design qa` scans local design surfaces for static
accessibility, responsive, and color token findings, then writes
`design-visual-diff-report.json` from deterministic source visual signatures
between runs. Rendered screenshot pixel diff and gpt-image-2 review remain
non-live.
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

- Remote provider API adapters and live OAuth/Keychain integration
- External MCP client/server transports beyond the local stdio JSON-RPC
  one-shot surface
- Full Playwright browser control, screenshots, clicks, typing, and DOM capture
- Rendered screenshot visual diff and image-generation design review
- Dynamic dependency vulnerability resolution and sandboxed exploit testing
- Desktop mouse/keyboard action execution beyond brokered policy decisions
- macOS accessibility/app automation beyond brokered inspection and inventory
- Provider-backed worker execution, repair waves, and final apply transactions
- Native/live Tauri GUI beyond the static dashboard artifact
- Production crypto/notarized updater apply, network install/apply, and
  production-grade acceptance targets
