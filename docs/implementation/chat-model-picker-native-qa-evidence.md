# Chat Model Picker Native QA Evidence - 2026-06-26

## Scope

- Commit under review: `4796ae4 Fix Chat model picker refresh`
- Workspace: `/Users/weklem/Desktop/devs/opensks`
- Conversation under native UI proof: `374e912c603d1a9d7649ddc3d4632c79`
- Reason for this artifact: SKS `$Naruto` follow-up required native multi-session evidence and durable proof beyond prose-only ledger notes.

## Native UI Proof Trace

Computer Use read the native app after launching `.opensks/macos/OpenSKS.app/Contents/MacOS/OpenSKS` directly because LaunchServices `open` still returned the known `kLSNoExecutableErr` packaging gap.

Relevant `app_state` excerpt:

```text
App=/Users/weklem/Desktop/devs/opensks/.opensks/macos/OpenSKS.app/ (bundleID dev.opensks.local, pid 67008)
Window: "OpenSKS", App: OpenSKS.
3 text OpenSKS Studio ~/Desktop/devs/opensks Idle · agent ready
5 text Help: Configured provider registry connections — secrets are never shown, Value: 1 provider
8 button Description: Chat, Help: Chat (⌘2), ID: rail.tile.chat
28 button (selected) Description: Conversation New conversation, Failed, ID: conversation.row.374e912c603d1a9d7649ddc3d4632c79
49 menu button Model Auto code model, ID: workspace.central.chat
50 menu button Image Auto, ID: workspace.central.chat
57 text ~/Desktop/devs/opensks 22 passed 1 partial 0 failed In progress 14 missions · 424 voxels
```

Interpretation: the native Chat composer is no longer stuck at `Model Auto`; it renders the persisted pinned registry-backed model label as `Model Auto code model`.

## Durable Settings Proof

Command:

```bash
.opensks/macos/OpenSKS.app/Contents/Resources/opensks-cli conversation settings-get --workspace /Users/weklem/Desktop/devs/opensks --conversation 374e912c603d1a9d7649ddc3d4632c79
```

Output:

```json
{
  "approval_policy_id": "safe-interactive",
  "conversation_id": "374e912c603d1a9d7649ddc3d4632c79",
  "execution_mode": "worktree",
  "max_parallelism": 4,
  "model_selection": {
    "mode": "pinned",
    "model_id": "provider-4d6d0c70-6b83-4047-9bf1-158e2f98a194/auto-code"
  },
  "pipeline_id": "auto",
  "reasoning_effort": "standard",
  "schema": "opensks.thread-settings.v1",
  "tool_policy_id": "project-default",
  "updated_at_ms": 1782454623009,
  "verifier_count": 1
}
```

Interpretation: the same conversation's durable thread settings are pinned to the provider registry model that the native composer now displays.

## Native Session Evidence

- Lane 1 `019f0292-75de-78e1-90cf-b055a05db040` (`Verifier`): read-only native reviewer for Swift decode/store/composer files. Scope: `ConversationModels.swift`, `ConversationStore.swift`, `ConversationComposer.swift`. Findings: none. It inspected the focused diff, ran `swift test --package-path swift --scratch-path /private/tmp/opensks-review-4796ae4 --filter ConversationUITests/testThreadSettings`, and reported 3 tests passed. Outcome: pass for targeted code safety.
- Lane 2 `019f0292-7823-7711-a9b9-8c708c8f5b26` (`Reviewer`): read-only native reviewer for tests/ledger evidence. Scope: `ConversationUITests.swift` and the implementation ledger. It reran `ConversationUITests` (25 passed), `ConversationTurnTests/testSendPassesThreadSettingsAndContextRefs` (1 passed), `cargo fmt --all --check`, `git diff --check`, and `swift-format lint` (exit 0 with existing warning backlog). Findings: evidence was insufficient at that time because the ledger lacked five-lane proof, durable GUI proof, and a direct rendered-chip test. Resolution: this artifact adds the durable UI/CLI proof and records lanes 1-5; the direct rendered-chip test remains a residual improvement because the native app_state proof covers the user-visible chip.
- Lane 3 `019f0299-f2ba-7a93-a6af-a84a9c60b654` (`Reviewer the 2nd`): read-only native reviewer for durable UI/CLI proof sufficiency. It inspected `git status --short --untracked-files=all`, `git show --stat --name-status 4796ae4`, this proof file, relevant Swift files, exact bundled CLI `conversation settings-get`, `git diff --check`, and `git ls-files`. Findings: proof was not yet durable in Git, lanes 3-5 had not yet been recorded, and the app_state excerpt lacked an external transcript ID. Resolution: this follow-up records lanes 3-5 here and stages/commits the proof file; the raw Computer Use transcript remains unavailable as an exported artifact, so this document includes the relevant app_state excerpt and matching CLI JSON.
- Lane 4 `019f0299-f4e1-7d42-8268-1c3e4049b063` (`Verifier the 2nd`): read-only native reviewer for automated coverage. It ran `swift test --disable-sandbox --package-path swift --scratch-path /tmp/opensks-swift-build-review-lane4-ui --filter OpenSKSStudioTests.ConversationUITests` (25 passed) and `swift test --disable-sandbox --package-path swift --scratch-path /tmp/opensks-swift-build-review-lane4-turn --filter OpenSKSStudioTests.ConversationTurnTests/testSendPassesThreadSettingsAndContextRefs` (1 passed). Findings: the UI decode/hydration/publish tests support the picker fix, but ledger wording overstated the turn test as proving a compatibility settings echo. Resolution: the ledger row was corrected to state that `ConversationStore.send` forwards `thread_settings_updated_at_ms` plus context refs while intentionally leaving the legacy settings echo nil.
- Lane 5 `019f0299-f6c8-7730-8959-b84867ebeaf8` (`Reviewer the 3rd`): read-only native reviewer for git isolation, user-change preservation, and secret exposure. It inspected `git rev-parse HEAD`, `git show --no-patch --pretty=fuller 4796ae4`, `git status --short --branch --untracked-files=all`, `git diff --name-status`, `git diff --cached --name-status`, `git show --name-status --stat 4796ae4`, `git show --check 4796ae4`, `git diff --check`, `git ls-files --others --exclude-standard`, `stat`, `wc -l`, `sed`, `nl -ba`, and targeted `rg` searches. Findings: git isolation and user-change preservation passed; no credential leak found. It still blocked final Naruto readiness at that time because five-lane evidence had not yet been recorded. Resolution: this section now records all five lanes and preserves the unrelated backup file untouched.

Five-lane aggregation status after this update: complete for native read-only reviewer/QA evidence. Lanes 2-5 reported proof/wording gaps while the aggregation was still incomplete; those gaps are resolved by this artifact, by the ledger correction, and by committing this proof. No lane requested a code change to the Chat picker implementation after commit `4796ae4`.

## Code-Changing Worker Split Decision

No post-commit code-changing worker lane was spawned for this follow-up because the bug fix was already integrated in commit `4796ae4` and all changed production/test files are tightly coupled to the same Chat settings decode/refresh path. Splitting additional code-changing workers after integration would require multiple agents to touch the same Swift files, increasing conflict and regression risk. The safer `$Naruto` degradation path is parent-owned integration plus independent native read-only reviewer/QA lanes.

## Screenshot Note

Two attempted `screencapture` files were rejected before staging because the Codex app window, not the OpenSKS app window, was captured. The invalid screenshot artifact was deleted and is not used as evidence. The durable UI proof is the Computer Use accessibility trace above plus the matching CLI settings JSON.
