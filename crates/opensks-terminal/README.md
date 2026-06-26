# OpenSKS Terminal Runtime

Local PTY-backed terminal runtime for OpenSKS.

This crate owns shell process execution, PTY input/output, resize, stop, command
block framing, conservative risk classification, redaction, and local runtime
artifacts. It does not add daemon routing, CLI commands, Swift UI, or
provider-backed AI suggestions.

Raw PTY output is written only under local ignored paths:

- `.opensks/runtime/terminal/sessions/<session-id>/output.raw`
- `.opensks/logs/terminal/<yyyy-mm-dd>.jsonl`

Shareable command blocks use contract DTOs with redacted commands and digests.
They do not copy raw transcript bytes into tracked/shared paths.

## PTY Dependency

This implementation uses `portable-pty = "0.9"`.

Reasoning:

- It exposes a small cross-platform Rust API around native PTY systems.
- The documented API supports `native_pty_system`, `openpty`, `CommandBuilder`,
  `MasterPty::try_clone_reader`, `MasterPty::take_writer`, `MasterPty::resize`,
  and child `try_wait`/`wait`/`kill` operations needed by the runtime.
- It supports macOS and Linux for the MVP. Windows is treated as unsupported by
  this runtime until OpenSKS explicitly defines Windows PTY behavior.

## Contract Dependency

This crate intentionally refers to terminal DTOs from `opensks-contracts`.
If the terminal contract branch has not been merged yet, do not create local
placeholder DTOs here. Keep the contract dependency as the single source of
truth and resolve the `TODO(contract)` integration by rebasing onto the contract
branch.
