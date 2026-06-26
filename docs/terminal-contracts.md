# OpenSKS Terminal Contracts

This document is the shared contract surface for the OpenSKS AI Terminal MVP.
It defines JSON keys, enum labels, schema ids, artifact boundaries, and safety
rules before PTY execution, provider-backed suggestions, daemon routing, or
Swift UI are implemented.

## Feature Overview

The terminal contract separates three concerns:

- A terminal session is local runtime state.
- An AI or catalog suggestion is a proposal only.
- Shell execution is a later user action, guarded by a risk decision.

Raw terminal output and raw transcripts are local-only. Shared durable artifacts
may contain redacted summaries, command digests, schema files, and merge-friendly
catalog entries, but not machine-specific paths, private env values, or raw
provider responses.

Provider-backed AI may be unavailable. Contracts and UI must represent that as
`provider_available=false` or `not_connected`, not as a completed AI feature.

## Request Kinds

`EngineRequestKind` owns the terminal request entrypoints:

- `terminal_session_start`
- `terminal_session_input`
- `terminal_session_resize`
- `terminal_session_stop`
- `terminal_suggestion_request`
- `terminal_agent_turn_start`

The request kind is always snake_case on the wire. Unknown enum labels must be
tolerated by clients and mapped to a local unknown/fallback case where possible.

## Session Start

```json
{
  "schema": "opensks.engine-request.v1",
  "id": "req-terminal-start-1",
  "kind": "terminal_session_start",
  "protocol_version": "opensks.contracts.v1",
  "params": {
    "terminal_session_start": {
      "schema": "opensks.terminal-session.v1",
      "session_id": "terminal-session-1",
      "cwd": "/Users/example/project",
      "shell": "zsh",
      "env_policy": "deny_secrets",
      "cols": 120,
      "rows": 32,
      "started_by": "swift_ui"
    }
  }
}
```

`cols` normalizes to `20..=500`; `rows` normalizes to `5..=200`.

## Session Input

```json
{
  "schema": "opensks.engine-request.v1",
  "id": "req-terminal-input-1",
  "kind": "terminal_session_input",
  "protocol_version": "opensks.contracts.v1",
  "params": {
    "terminal_session_input": {
      "schema": "opensks.terminal-session.v1",
      "session_id": "terminal-session-1",
      "text": "cargo test -p opensks-contracts\n",
      "input_kind": "user_command",
      "approved_risk_decision_id": null
    }
  }
}
```

`agent_proposed_command` is not shell execution. It is still input that must be
approved or accepted by the user before runtime executes it.

## Resize And Stop

```json
{
  "schema": "opensks.engine-request.v1",
  "id": "req-terminal-resize-1",
  "kind": "terminal_session_resize",
  "protocol_version": "opensks.contracts.v1",
  "params": {
    "terminal_session_resize": {
      "schema": "opensks.terminal-session.v1",
      "session_id": "terminal-session-1",
      "cols": 100,
      "rows": 30
    }
  }
}
```

```json
{
  "schema": "opensks.engine-request.v1",
  "id": "req-terminal-stop-1",
  "kind": "terminal_session_stop",
  "protocol_version": "opensks.contracts.v1",
  "params": {
    "terminal_session_stop": {
      "schema": "opensks.terminal-session.v1",
      "session_id": "terminal-session-1",
      "reason_code": "closed_by_user"
    }
  }
}
```

## Suggestion Request

```json
{
  "schema": "opensks.terminal-suggestion-request.v1",
  "request_id": "suggestion-request-1",
  "cwd": "/Users/example/project",
  "input": "cargo te",
  "cursor": 8,
  "shell": "zsh",
  "last_exit_code": 101,
  "max_suggestions": 5,
  "include_ai": false,
  "context_refs": [
    ".opensks/wiki/records/terminal-command-catalog.md"
  ]
}
```

`max_suggestions` normalizes to `1..=20`. `include_ai=true` means provider
proposals may be included when a provider is connected; it does not authorize
auto-execution.

## Suggestion

```json
{
  "schema": "opensks.terminal-suggestion.v1",
  "id": "suggestion-1",
  "replacement": "cargo test -p opensks-contracts",
  "display": "cargo test -p opensks-contracts",
  "description": "Run the focused contract package tests.",
  "source": "project_catalog",
  "confidence": 0.92,
  "risk": "safe",
  "requires_approval": false,
  "evidence_refs": [
    ".opensks/wiki/records/terminal-command-catalog.md"
  ]
}
```

Suggestions may come from shell history, completion, project catalog, file path
completion, OpenSKS context, provider output, or fallback logic. A provider
suggestion is still a proposal.

## Risk Decision

```json
{
  "schema": "opensks.terminal-risk-decision.v1",
  "id": "risk-1",
  "command_redacted": "cat <redacted-secret-file>",
  "risk": "secret_exposure",
  "decision": "block",
  "reason_code": "terminal_risk_policy_default",
  "requires_approval": true,
  "evidence_refs": []
}
```

Default policy:

- `safe` may be allowed.
- `caution` may warn.
- `destructive`, `privileged`, `network_mutation`, and `unknown` require approval.
- `secret_exposure` blocks by default.

## Command Block

```json
{
  "schema": "opensks.terminal-command-block.v1",
  "block_id": "block-1",
  "session_id": "terminal-session-1",
  "cwd_redacted": "~/project",
  "command_redacted": "cargo test -p opensks-contracts",
  "started_at_ms": 1782477600000,
  "finished_at_ms": 1782477601234,
  "exit_code": 0,
  "stdout_digest": "sha256:example-stdout-digest",
  "stderr_digest": null,
  "redacted": true,
  "evidence_refs": [
    ".opensks/runtime/terminal/sessions/terminal-session-1/blocks.jsonl"
  ]
}
```

Command blocks store redacted command text and digests only. Raw stdout/stderr
stay in local runtime or local logs according to the manifest.

## Agent Turn

```json
{
  "schema": "opensks.engine-request.v1",
  "id": "req-agent-turn-1",
  "kind": "terminal_agent_turn_start",
  "protocol_version": "opensks.contracts.v1",
  "params": {
    "terminal_agent_turn_start": {
      "schema": "opensks.terminal-agent-turn.v1",
      "turn_id": "terminal-agent-turn-1",
      "session_id": "terminal-session-1",
      "cwd": "/Users/example/project",
      "prompt": "Explain why the previous cargo test failed.",
      "mode": "diagnose_failure",
      "allow_command_proposals": true,
      "allow_auto_execute_safe_commands": false,
      "context_refs": [
        ".opensks/runtime/terminal/sessions/terminal-session-1/blocks.jsonl"
      ]
    }
  }
}
```

`allow_auto_execute_safe_commands` exists only as contract data in this MVP and
must be used as `false` by CLI/UI until a later execution policy implements it.

## Event

```json
{
  "schema": "opensks.terminal-event.v1",
  "event_id": "terminal-event-1",
  "session_id": "terminal-session-1",
  "event_kind": "command_finished",
  "sequence": 12,
  "timestamp_ms": 1782477601234,
  "command_block": {
    "schema": "opensks.terminal-command-block.v1",
    "block_id": "block-1",
    "session_id": "terminal-session-1",
    "cwd_redacted": "~/project",
    "command_redacted": "cargo test -p opensks-contracts",
    "started_at_ms": 1782477600000,
    "finished_at_ms": 1782477601234,
    "exit_code": 0,
    "stdout_digest": "sha256:example-stdout-digest",
    "stderr_digest": null,
    "redacted": true,
    "evidence_refs": []
  },
  "output_digest": "sha256:example-output-digest",
  "message_redacted": "Command completed.",
  "redacted": true,
  "evidence_refs": []
}
```

## Artifact Path Policy

Local-only terminal paths:

- `.opensks/runtime/terminal/`
- `.opensks/logs/terminal/`

Shared durable terminal path:

- `.opensks/wiki/records/terminal-command-catalog.md`

The shared command catalog is merge-friendly project knowledge. It must not
contain raw output, absolute local paths, provider responses, or secrets.

## Local-Only Versus Shared-Durable

Local-only data may include PTY state, raw transcript fragments, command block
JSONL, suggestion cache, and redacted local logs. These paths are ignored by Git
and may contain machine-local paths when required for runtime operation.

Shared-durable data may include generated schemas, docs, redacted summaries,
digests, and the command catalog. Shared data must be portable and secretless.

## Swift Decoding Policy

Swift should decode known enum labels into typed cases and preserve unknown
labels in an unknown case or raw string wrapper. Unknown terminal labels must
not crash the app, drop the entire payload, or make pending requests hang.

For `provider_available=false`, Swift should show the terminal contract and
catalog suggestions as available while clearly marking provider-backed AI as not
connected.

## Daemon Completion Marker Policy

Every daemon request that accepts terminal JSON must still complete with the
existing explicit per-request `request_completed` marker. Clients must complete
responses on that marker, not on a quiet-window heuristic.

Terminal events may stream before the completion marker, but the marker remains
the authoritative end of that request response.

## Approval Policy

AI-created commands are suggestions. Runtime may execute only after a separate
user input or approval event. Destructive, privileged, secret-exposure,
network-mutation, and unknown risks must not auto-execute. Secret exposure is
blocked by default.

## Not In MVP

- No provider-backed command auto-execution.
- No remote shell.
- No secret env introspection.
- No full terminal replay cloud sync.
- No dangerous command silent execution.
- No PTY execution in the contract branch.
- No Swift terminal UI in the contract branch.
