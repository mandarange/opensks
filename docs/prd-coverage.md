# PRD v3 Coverage Ledger

This repository is being implemented against:

```text
/Users/weklem/Desktop/opensks_prd_v3_goal_loop_mcp_computer_use_voxel_triwiki.md
```

The CLI can generate the machine-readable ledger:

```bash
cargo run -- prd coverage
```

The resulting `.opensks/prd-coverage.json` uses these statuses:

```text
implemented
artifact_mvp
planned_artifact
missing_live_implementation
```

Current interpretation:

- `implemented`: present as executable Rust behavior.
- `artifact_mvp`: present as a proof-first artifact-producing MVP path.
- `planned_artifact`: represented by a command, schema, registry, or audit artifact, but not live integrated.
- `missing_live_implementation`: not complete by the PRD acceptance criteria.

`requirement-coverage-gate.json` uses this ledger to evaluate PRD section 18
production criterion `requirement coverage >= 95%`. Its numerator is
`implemented + artifact_mvp`; it explicitly sets `live_acceptance_all_passed` to
`false`. Therefore `prod-003: passed` means artifact-backed PRD requirement
coverage is above threshold, not that the product or all live production
acceptance criteria are complete.

`prod-005: passed` has the same scoped meaning, but it is evidence-bound:
`acceptance audit` reads the latest mission `final-seal.json` and only marks it
passed when `artifact_mvp_final_seal_integrity_status` is `passed` and checked
artifact refs exist. If no such final seal is present, `prod-005` remains
partial. Even when the scoped artifact contract passes, `live_route_completion`,
live H-proof, provider-backed workers, repair waves, and final apply remain
explicitly false.

`prod-002: passed` is also scoped to local artifact evidence. `acceptance audit`
binds it to `.opensks/scheduler/*/stage-overlap-report.json` and treats the
local scheduler stage-overlap target as met only when `target_met=true`,
`observed_parallel_execution=true`, `overlap_ratio>=target_ratio`, and every
recorded stage span passed. This does not mean provider-backed or production
worker overlap tuning is live; that tuning remains a non-goal for the current
slice and a remaining production gap.

`prod-006: passed` is scoped to local signed-updates artifact evidence under
`.opensks/updater`: `update-manifest.json`, `update-signature.json`,
`update-channels.json`, `rollback-plan.json`, `update-boundary.json`, and
`updater-final-state.json`. The scoped gate requires the manifest to require
signature and rollback; the signature `manifest_hash` to match the manifest
hash; the signature value to equal `local_update_signature(manifest_hash)`;
final state to report `signature_verified=true` and
`network_or_install_performed=false`; stable/latest channels to require
signature and rollback; the update boundary to require operator approval,
verified signature, and rollback; and rollback apply transaction to remain
non-live. This is not production crypto, notarization, network install, or live
apply evidence; those remain production gaps.

Current live local slices:

- `qa run` executes `cargo fmt --check`, `cargo test --no-run`, and `cargo clippy --all-targets --all-features -- -D warnings` when `Cargo.toml` exists.
- `qa run` also performs a built-in workspace secret scan and writes `secret-leak-rate.json`, `secret-leak-gate.json`, and `secret-leak-release-history.json` as the current workspace release zero-leak gate with a local release-history denominator.
- `security audit` writes a threat model plus static security findings for prompt injection, MCP allowlist bypass phrasing, supply-chain shell pipes, unsafe actions, and secrets, along with the same `secret-leak-rate.json`, `secret-leak-gate.json`, and `secret-leak-release-history.json` artifacts under `.opensks/security`. These artifacts keep `live_external_production_telemetry=false`; the pass scope is local release-history evidence, not an external production telemetry feed.
- `cache warm` hashes local text-like cache segments, classifies stable versus dynamic context, includes deterministic Voxel TriWiki context in the stable prefix when `voxel index` has written `.opensks/triwiki/voxels.jsonl`, and writes `cache-hit-report.json` plus `.opensks/cache/cache-layout-improvement.json` by comparing the current stable prefix with the previous warm snapshot. The beta-004 "Voxel TriWiki improves cache layout" slice is scoped only to that local `opensks.cache-layout-improvement.v1` artifact: `scope=voxel_triwiki_cache_layout`, `strategy=stable_prefix_dynamic_suffix`, `layout_gate_passed=true`, `baseline_available=true`, `voxel_triwiki_segment_present=true`, and `local_warm_prefix_hit_percent >= target_hit_percent`. It must also keep `provider_metrics_available=false` and `live_provider_cache_metrics=false`, so `prod-001` passes only for local stable-prefix reuse evidence while provider/runtime cache-layout improvement remains unverified. The Voxel TriWiki cache-layout gate requires the `.opensks/triwiki/voxels.jsonl` segment; stable-prefix reuse alone is not enough for that scoped gate.
- The planned/implemented beta-005 "Token dashboard tracks provider cache hit" pass is local artifact evidence only: `cache-hit-report.json`, `cache-dashboard.json`, and `providers/usage-dashboard.json` track provider cache-hit fields, source/status, and estimated cached tokens. Live provider cached-token metrics remain unavailable/not connected, so the dashboard artifacts do not prove live provider telemetry.
- `bench` records timed local runtime checks plus scoped local/native multi-session collaboration evidence artifacts: it can write `native-collaboration-execution.json` and `native-collaboration-events.jsonl` from `.sneakoscope` native agent session evidence. This does not satisfy `beta-006` without independently verifiable native-session provenance, and it does not claim live remote multi-provider API worker collaboration, provider credentials, hidden fallback, or final apply.
- `auth` discovers provider environment-variable configuration without exposing secret values and writes auth policy plus audit artifacts for Keychain-first storage posture, OAuth candidates, API keys, and local endpoints.
- `provider list`, `provider probe`, `provider usage`, and `provider adapter-check` write provider profiles, first-class/optional adapter capabilities, local endpoint reachability probes, OpenRouter/OpenAI adapter smoke evidence, and zero-leak usage counters.
- `updater plan` writes the `prod-006` local signed-updates artifact set under `.opensks/updater` and verifies local manifest-signature, channel, boundary, rollback, and final-state evidence without performing production crypto, notarization, network install, or live apply.
- `acceptance audit` writes MVP/Beta/Production acceptance ledgers and findings so remaining live gaps are explicit rather than inferred from green tests.
- `prd coverage` writes `prd-coverage.json` and `requirement-coverage-gate.json`, checking implemented plus artifact-MVP PRD requirement coverage against the 95% production threshold while keeping live acceptance completion separate.
- `voxel index` scans workspace text into code, symbol, design, security, provider, package, and context voxels with stable/dynamic cache classification.
- Goal missions write `automation-loop.json` to represent goal analysis, context composition, work decomposition, QA, repair, final apply, report, and self-improve stages with explicit live/artifact status.
- Goal missions write `goal-kind-registry.json` with every PRD section 2.3 goal kind and the selected kind for the run.
- `browser` brokers safe network observation, extracts title/hash/link/form/meta evidence for HTTP(S) targets, and writes HAR-like/final-state/action-plan/policy-decision artifacts. The scoped `mvp-007` pass is local deterministic browser runtime artifact evidence only: it writes `browser-runtime/index.html`, `browser-interaction-loop.json`, `browser-interaction-events.jsonl`, `browser-session.json` / `session-summary.json` bindings, and local PPM screenshot artifacts for open/screenshot/click/type evidence produced by `browser`. It does not claim live Playwright/Chrome Extension/browser control, live DOM interaction, real browser-rendered screenshots, external web control, or credential entry.
- `computer-use` brokers safe observation, blocks or marks mouse/keyboard and sensitive actions for approval, and writes screenshot/final-state/action-plan/policy-decision artifacts. The scoped `beta-002` pass is local isolated computer/browser observation-loop evidence only: it requires `isolated-browser-container.json`, `computer-browser-loop.json`, `computer-browser-loop-events.jsonl`, `isolated-browser-runtime/index.html`, policy evidence, and final-state evidence. Deterministic synthetic local HTML open/click/type event artifacts are recorded with policy approval boundaries; this does not claim live browser container control, live mouse/keyboard execution, external web control, or arbitrary browser automation.
- `app-use` brokers native app intents for the scoped `mvp-008` pass, "App use can inspect macOS accessibility tree", as local artifact evidence only. It writes `accessibility-tree.json`, `running-apps.json`, `app-final-state.json`, and policy/action-plan artifacts. The scoped acceptance check requires a captured accessibility-tree node or frontmost app, a running-app inventory, inspection attempted, policy allowed inspection, and `live_app_actions_executed=false`; it does not prove full native app action execution, arbitrary UI control, or live macOS app automation.
- `app` writes a static `.opensks/app/dashboard.html` plus `gui-data.json` and `worker-lanes.json` from local PRD, QA/security, Voxel TriWiki, provider, mission status, worker-lane, and use-plane artifacts, and also writes platform, module, macOS integration, source-note, and product-statement manifests.
- `design qa` scans local design surfaces and records static accessibility/responsive/color-token findings. The scoped `beta-003` pass is deterministic local raster screenshot artifact plus pixel diff evidence only: it writes generated local PPM screenshot artifacts, `design-screenshot-snapshots.jsonl`, and `design-screenshot-diff-report.json` from local renderer state between runs. It does not claim live browser-rendered screenshot capture, Chrome Extension evidence, gpt-image-2/ImageGen review, Product Design plugin visual comparison, or external design service execution.
- `mcp audit` writes a broker policy that denies raw model tool calls by default.
- `mcp describe`, `mcp invoke`, and `mcp serve --once` provide a local MCP-style JSON-RPC surface for allowlisted OpenSKS tools.
- `scheduler run` writes a bounded local scheduler plan, event stream, final state, and local `stage-overlap-report.json` from concurrent runtime metadata checks, which is the artifact scope for the `prod-002` local scheduler overlap target.
- `worktree create` creates an isolated snapshot under `.opensks/worktrees/.../workspace`.
- `patch propose` writes a patch envelope plus gate result that blocks final apply until real checks pass.

The project is not complete until the acceptance criteria in PRD section 18 are
proven by live runtime behavior, not just scaffold artifacts.
