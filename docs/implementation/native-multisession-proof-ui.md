# Native Multisession Proof UI Evidence

## Chat failed-details popover

Computer Use inspected the native OpenSKS app accessibility tree for the selected failed Chat run. Activating the failed-run details control exposed a popover whose accessibility subtree includes `run.failureDiagnostics.popover`.

Evidence observed in the tree:
- `run.failureDiagnostics.popover` contains a scroll area with `Scroll Up` and `Scroll Down` secondary actions.
- The failure summary is visible as `Summary TurnSupervisor failed.`
- Reason/code signals are visible, including `git_worktree_created`, `provider_call_failed`, and `turn_supervisor_failed`.
