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

Current live local slices:

- `qa run` executes `cargo fmt --check`, `cargo test --no-run`, and `cargo clippy --all-targets --all-features -- -D warnings` when `Cargo.toml` exists.
- `qa run` also performs a built-in workspace secret scan.
- `security audit` writes a threat model plus static security findings for prompt injection, MCP allowlist bypass phrasing, supply-chain shell pipes, unsafe actions, and secrets.
- `cache warm` hashes local text-like cache segments and classifies stable versus dynamic context.
- `bench` records timed local runtime checks.
- `auth` discovers provider environment-variable configuration without exposing secret values.
- `provider list`, `provider probe`, and `provider usage` write provider profiles, local endpoint reachability probes, and zero-leak usage counters.
- `voxel index` scans workspace text into code, symbol, design, security, provider, package, and context voxels with stable/dynamic cache classification.
- Goal missions write `goal-kind-registry.json` with every PRD section 2.3 goal kind and the selected kind for the run.
- `browser` brokers safe network observation, extracts title/hash/link/form/meta evidence for HTTP(S) targets, and writes HAR-like/final-state/action-plan/policy-decision artifacts.
- `computer-use` brokers safe observation, blocks or marks mouse/keyboard and sensitive actions for approval, and writes screenshot/final-state/action-plan/policy-decision artifacts.
- `app-use` brokers native app intents, captures frontmost/running-app inspection state, and blocks or marks sensitive native actions for approval.
- `app` writes a static `.opensks/app/dashboard.html` plus `gui-data.json` from local PRD, QA/security, Voxel TriWiki, provider, mission, and use-plane artifacts.
- `design qa` scans local design surfaces and records static accessibility, responsive, and color-token findings.
- `mcp audit` writes a broker policy that denies raw model tool calls by default.
- `mcp describe`, `mcp invoke`, and `mcp serve --once` provide a local MCP-style JSON-RPC surface for allowlisted OpenSKS tools.
- `scheduler run` writes a bounded local scheduler plan, event stream, and final state.
- `worktree create` creates an isolated snapshot under `.opensks/worktrees/.../workspace`.
- `patch propose` writes a patch envelope plus gate result that blocks final apply until real checks pass.

The project is not complete until the acceptance criteria in PRD section 18 are
proven by live runtime behavior, not just scaffold artifacts.
