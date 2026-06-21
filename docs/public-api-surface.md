# OpenSKS Public API Surface

This document tracks the current Rust public API while `src/lib.rs` is being
reduced to a compatibility facade. It is intentionally narrow: items listed here
are callable from downstream crates or the binary, not a claim that the whole
OpenSKS Next directive is complete.

## Root Compatibility Facade

The root crate currently keeps these public items:

- `ExecutionMode`
- `GoalRunConfig`
- `GoalRunResult`
- `CliOutput`
- `OpenSksError`
- `cli_error_json`
- `run_cli`
- `is_daemon_stdio_invocation`
- `run_daemon_stdio_stream`
- `start_goal_loop`
- `default_cwd`
- `native_app_bundle_path`
- `default_launch_cwd`
- `current_app_bundle_workspace`
- `open_path_for_user`

Long term, `run_cli`, `CliOutput`, daemon command handling, app launch helpers,
and goal loop entrypoints should continue moving into owned crates. The root
crate remains a migration facade until that extraction is complete.

## CLI Crate-Owned Command Surfaces

`opensks-cli` currently owns these command implementations or command helpers:

- `daemon --stdio`
- `history init`
- `graph templates|compile`
- `hooks replay`
- `codegraph index|query`
- `triwiki seed`
- `context pack`
- `worktree create|isolate`
- `provider route code|text|image`
- `patch propose|check`
- `image ledger`
- `reasoning debate`
- `git outbox`
- `gc plan`
- `release proof`
- `scheduler run|simulate|dispatch|recover`
- `worker runtime`
- scheduler QA check/render helpers

These surfaces are the preferred place for new CLI behavior. New durable JSON
schemas should use typed structs and `serde` rather than new manual string
renderers in `src/lib.rs`.

## Root-Owned Compatibility Surfaces Remaining

The following command families still have substantial implementation in
`src/lib.rs` and are explicit extraction backlog:

- `goal`, `run`, and `naruto` compatibility flows
- `mcp`, `browser`, `computer-use`, and `app-use`
- `voxel`, `cache`, `qa`, `security`, `design`, and `bench`
- `auth`
- `provider list|probe|usage|adapter-check`
- `updater`, `prd coverage`, `acceptance audit`, and `app`
- native app bundle generation and app-data rendering

Manual JSON readers remain in `src/lib.rs` for migration and acceptance readers.
They should not be used for new durable schemas.
