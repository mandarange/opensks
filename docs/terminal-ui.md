# OpenSKS SwiftUI Terminal UI

The SwiftUI Terminal route is a daemon-backed operator surface. It is not a full
VT100 emulator and it does not silently execute AI-proposed commands.

## Terminal Tab UX

The primary navigation rail includes a `Terminal` destination. The route shows
the session `cwd`, shell name, daemon status, command block list, suggestion
list, agent messages, and an input row.

The existing code-workspace bottom drawer remains the lightweight output /
problems / activity drawer. The new Terminal route is the daemon-backed terminal
workspace.

## Keyboard Shortcuts

- `Return`: submit the input.
- `Tab`: accept the visible safe ghost suggestion into the input.
- `Right Arrow`: accept the visible safe ghost suggestion into the input.

Accepting a ghost suggestion only changes the input field. It does not run the
command.

## Suggestion Flow

Input changes are debounced before the view model sends
`terminal_suggestion_request` to the daemon. The first safe suggestion that does
not require approval becomes the ghost suggestion. Risky suggestions are shown in
the inline suggestion list with a risk badge.

The request envelope uses:

- `schema`: `opensks.engine-request.v1`
- `kind`: `terminal_suggestion_request`
- `protocol_version`: `opensks.contracts.v1`
- `params.terminal_suggestion_request.schema`:
  `opensks.terminal-suggestion-request.v1`

## Agent Prompt Flow

Input beginning with `/agent ` sends `terminal_agent_turn_start`. Natural
language input is also routed to the agent proposal path instead of being sent to
the shell. Agent proposals render as suggestions and are never executed unless
the user explicitly presses a run control.

Forced shell input begins with `!`; the leading bang is removed before sending
the command as terminal input.

## Risk And Approval Flow

Risk labels decode without failing the full response. Unknown risk labels become
`.unknown`.

Risk display:

- `safe`: no approval required.
- `caution`: warning badge.
- `privileged`: approval required.
- `secret_exposure`: approval required and blocked by default for direct run.
- `network_mutation`: approval required.
- `destructive`: approval required and blocked by default for direct run.
- `unknown`: approval required.

When approval is required, the UI shows a confirmation dialog with the redacted
command and risk label. The MVP supports `Cancel` and `Approve and Insert`.
Approval inserts the command into the input field; it does not bypass daemon
approval for immediate execution.

## Daemon Unavailable Behavior

If the daemon is unavailable, the route shows:

```text
Terminal daemon is not connected.
Run `cargo run -- terminal smoke` to verify the local runtime.
```

If AI/provider-backed proposals are unavailable, the route shows:

```text
AI command proposals are not connected yet. Deterministic suggestions are available.
```

If the platform is unsupported, the route shows:

```text
PTY terminal runtime is not supported on this platform yet.
```

Daemon request completion follows the existing engine bridge behavior: pending
responses complete only after the correlated `request_completed` marker.

## MVP Limitations

- No full VT100 terminal emulator.
- Command output is rendered as bounded, monospaced command blocks.
- ANSI escape sequences and secret-looking values are stripped/redacted for the
  preview.
- In-memory previews are bounded to 20,000 characters per block and 200 blocks.
- Persistent raw transcript replay is future work.

## Future Work

- Full terminal emulator integration.
- Persistent live terminal subscription.
- Provider-backed repair loop.
- Daemon approval integration for `Approve and Run`.
