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

Current live local slices:

- `qa run` executes `cargo fmt --check`, `cargo test --no-run`, and `cargo clippy --all-targets --all-features -- -D warnings` when `Cargo.toml` exists.
- `qa run` also performs a built-in workspace secret scan and writes `secret-leak-rate.json`, `secret-leak-gate.json`, and `secret-leak-release-history.json` as the current workspace release zero-leak gate with a local release-history denominator.
- `security audit` writes a threat model plus static security findings for prompt injection, MCP allowlist bypass phrasing, supply-chain shell pipes, unsafe actions, and secrets, along with the same `secret-leak-rate.json`, `secret-leak-gate.json`, and `secret-leak-release-history.json` artifacts under `.opensks/security`. These artifacts keep `live_external_production_telemetry=false`; the pass scope is local release-history evidence, not an external production telemetry feed.
- `cache warm` hashes local text-like cache segments, classifies stable versus dynamic context, includes deterministic Voxel TriWiki context in the stable prefix when `voxel index` has written `.opensks/triwiki/voxels.jsonl`, and writes `cache-hit-report.json` plus `cache-layout-improvement.json` by comparing the current stable prefix with the previous warm snapshot. The Voxel TriWiki cache-layout gate requires the `.opensks/triwiki/voxels.jsonl` segment; stable-prefix reuse alone is not enough for that scoped gate.
- `bench` records timed local runtime checks plus explicit multi-LLM roster, role-assignment, disagreement, quorum, and collaboration preflight artifacts with hidden fallback disabled.
- `auth` discovers provider environment-variable configuration without exposing secret values and writes auth policy plus audit artifacts for Keychain-first storage posture, OAuth candidates, API keys, and local endpoints.
- `provider list`, `provider probe`, `provider usage`, and `provider adapter-check` write provider profiles, first-class/optional adapter capabilities, local endpoint reachability probes, OpenRouter/OpenAI adapter smoke evidence, and zero-leak usage counters.
- `updater plan` writes stable/latest channel, local signature proof, update boundary, rollback, and final-state artifacts without performing a network install.
- `acceptance audit` writes MVP/Beta/Production acceptance ledgers and findings so remaining live gaps are explicit rather than inferred from green tests.
- `prd coverage` writes `prd-coverage.json` and `requirement-coverage-gate.json`, checking implemented plus artifact-MVP PRD requirement coverage against the 95% production threshold while keeping live acceptance completion separate.
- `voxel index` scans workspace text into code, symbol, design, security, provider, package, and context voxels with stable/dynamic cache classification.
- Goal missions write `automation-loop.json` to represent goal analysis, context composition, work decomposition, QA, repair, final apply, report, and self-improve stages with explicit live/artifact status.
- Goal missions write `goal-kind-registry.json` with every PRD section 2.3 goal kind and the selected kind for the run.
- `browser` brokers safe network observation, extracts title/hash/link/form/meta evidence for HTTP(S) targets, and writes HAR-like/final-state/action-plan/policy-decision artifacts.
- `computer-use` brokers safe observation, blocks or marks mouse/keyboard and sensitive actions for approval, and writes screenshot/final-state/action-plan/policy-decision plus isolated browser/container observation-loop artifacts.
- `app-use` brokers native app intents, captures frontmost/running-app inspection state, and blocks or marks sensitive native actions for approval.
- `app` writes a static `.opensks/app/dashboard.html` plus `gui-data.json` and `worker-lanes.json` from local PRD, QA/security, Voxel TriWiki, provider, mission status, worker-lane, and use-plane artifacts, and also writes platform, module, macOS integration, source-note, and product-statement manifests.
- `design qa` scans local design surfaces, records static accessibility/responsive/color-token findings, and writes `design-visual-diff-report.json` from deterministic source visual signatures between runs.
- `mcp audit` writes a broker policy that denies raw model tool calls by default.
- `mcp describe`, `mcp invoke`, and `mcp serve --once` provide a local MCP-style JSON-RPC surface for allowlisted OpenSKS tools.
- `scheduler run` writes a bounded local scheduler plan, event stream, final state, and live `stage-overlap-report.json` from concurrent runtime metadata checks.
- `worktree create` creates an isolated snapshot under `.opensks/worktrees/.../workspace`.
- `patch propose` writes a patch envelope plus gate result that blocks final apply until real checks pass.

The project is not complete until the acceptance criteria in PRD section 18 are
proven by live runtime behavior, not just scaffold artifacts.
