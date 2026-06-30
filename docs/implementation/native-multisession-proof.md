# Native Multisession Proof

Generated: 2026-06-30T02:11:57Z

This note binds the current OpenSKS goal evidence for native multi-session work,
parallel file-writing, failed-details UI verification, and relocatable macOS
installation. It is an implementation proof, not a production release claim.

## Native subagent evidence

Two native implementation workers ran in parallel with disjoint write scopes:

- Mason wrote `docs/implementation/native-multisession-proof-ui.md`.
- Builder wrote `docs/implementation/native-multisession-proof-install.md`.

The workers did not share a write target. Both files were created as untracked
documentation artifacts, so their scoped `git diff -- <path>` output is empty
until staged, while `git status --short -- <path>` shows `??`.

## Runtime worker evidence

`target/debug/opensks worker runtime --scratch-apply "goal live native multisession scratch source proof"`
created:

`/.opensks/workers/worker-runtime-1782785388988214000-2357`

The generated receipts reported:

- `status = passed`
- `daemon_visible_worker_bus = true`
- `concurrent_request_routing = true`
- `actual_file_write_count = 2`
- `parallel_worker_file_edit_windows_verified = true`
- `nonblocking_worker_result_handoff_verified = true`
- `all_file_write_hashes_verified = true`
- `scratch_apply_file_count = 2`
- `scratch_apply_verified = true`

The scratch project contains two source files written by separate worker lanes:

- `scratch-project/src/1_implementation-worker.rs`
- `scratch-project/src/2_qa-reviewer.rs`

Current limitation: these are local runtime workers and native Codex subagents.
The receipts still report `live_provider_workers = false` and
`live_remote_provider_bus = false`, so this must not be claimed as proof of live
remote-provider worker dispatch.

## Failed-details UI evidence

Computer Use opened a failed Chat run in the native OpenSKS app. The failed-run
details popover exposed `run.failureDiagnostics.popover` with a scroll area that
supports `Scroll Up` and `Scroll Down`. The panel showed the failure summary and
reason/code signals, including `git_worktree_created`, `provider_call_failed`,
and `turn_supervisor_failed`.

## Install evidence

`scripts/install-macos-local-smoke.sh --clean --archive` passed and created
`.opensks/macos/OpenSKS-local-macos.zip`. The install smoke receipt recorded:

- `archive_verified = true`
- `archive_relocatable = true`
- `archive_workspace_override_smoke_verified = true`
- `archive_codesign_verified = true`

Current limitation: this is an ad-hoc signed local archive. It is not a
production signed/notarized release.
