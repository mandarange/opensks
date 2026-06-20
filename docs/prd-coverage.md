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
- `cache warm` hashes local text-like cache segments and classifies stable versus dynamic context.
- `bench` records timed local runtime checks.
- `auth` discovers provider environment-variable configuration without exposing secret values.
- `browser` performs curl-based network and page probes for HTTP(S) targets, extracts title/hash/bytes, and writes HAR-like/final-state artifacts.
- `computer-use` attempts macOS screenshot capture and writes screenshot/final-state/action artifacts.
- `app-use` attempts macOS frontmost-app inspection and records the result.
- `mcp audit` writes a broker policy that denies raw model tool calls by default.
- `mcp describe`, `mcp invoke`, and `mcp serve --once` provide a local MCP-style JSON-RPC surface for allowlisted OpenSKS tools.
- `scheduler run` writes a bounded local scheduler plan, event stream, and final state.
- `worktree create` creates an isolated snapshot under `.opensks/worktrees/.../workspace`.
- `patch propose` writes a patch envelope plus gate result that blocks final apply until real checks pass.

The project is not complete until the acceptance criteria in PRD section 18 are
proven by live runtime behavior, not just scaffold artifacts.
