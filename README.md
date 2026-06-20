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
cargo run -- voxel query "goal"
cargo run -- cache warm
cargo run -- qa run
cargo run -- design qa
cargo run -- bench
cargo run -- auth
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
progress-ledger.json
stop-policy.json
tool-plan.json
voxel-triwiki.json
voxels.jsonl
final-seal.json
prd-coverage.json
```

The final seal is intentionally marked `partial`: the current implementation
proves intake, artifact writing, capability planning, Voxel TriWiki seeding,
and PRD coverage accounting. Goal runs also write local scheduler, QA/security,
worktree-isolation, and patch-gate artifacts. `qa run` executes local Rust
checks when a Cargo workspace is present and always runs the built-in secret
scan. `cache warm` hashes local cache segments, `bench` records timed local
runtime checks, `auth` discovers configured provider environment variables
without exposing values, and `browser` performs curl network/page probes for
HTTP(S) targets. `computer-use` attempts macOS screenshot capture and records
the action ledger. Non-goal computer/app capability commands still create the
PRD-named audit/session artifacts with explicit non-live status where the full
engine does not exist yet, though `app-use` attempts a macOS frontmost-app
inspection. `mcp describe`, `mcp invoke`, and `mcp serve --once` expose a
local brokered MCP-style JSON-RPC surface for allowlisted OpenSKS tools such as
workspace search, Voxel query, final-seal reads, and local QA.

## PRD Coverage State

`cargo run -- prd coverage` writes `.opensks/prd-coverage.json`. The ledger is
not a completion claim; it is the current source of truth for what is already
implemented, what has an artifact-backed scaffold, and what still needs live
runtime work.

Still not live:

- Provider API adapters and OAuth/Keychain integration
- External MCP client/server transports beyond the local stdio JSON-RPC
  one-shot surface
- Full Playwright browser control, screenshots, clicks, typing, and DOM capture
- Desktop mouse/keyboard action execution beyond screenshot capture
- macOS accessibility/app automation beyond frontmost-app inspection
- Provider-backed worker execution, repair waves, and final apply transactions
- Tauri GUI
- Signed updater and production-grade acceptance targets
