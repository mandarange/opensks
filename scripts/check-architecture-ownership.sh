#!/usr/bin/env bash
#
# PR-020 Baseline Seal — architecture ownership and path-hygiene guard.
#
# Purpose: keep new domain logic out of the root monolith (src/lib.rs) and keep
# developer-machine paths / forbidden markers out of tracked sources. This script
# is the single source of truth for the "Architecture and path guard" CI step;
# it must pass on the baseline and fail on a fixture violation.
#
# Usage (from anywhere): scripts/check-architecture-ownership.sh
# Exit code: 0 = all checks passed, 1 = a guard tripped.
#
# Caps live in scripts/architecture-ownership.config. See
# docs/architecture-ownership.md for the ownership map and exception policy.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CONFIG="scripts/architecture-ownership.config"
if [ ! -f "$CONFIG" ]; then
  echo "ARCH-GUARD FAIL: missing $CONFIG" >&2
  exit 1
fi
# shellcheck source=/dev/null
. "$CONFIG"

fail() { echo "ARCH-GUARD FAIL: $*" >&2; exit 1; }
ok()   { echo "ARCH-GUARD ok:  $*"; }

# 0. Monotonic architecture budget. The legacy shell caps below remain for
# compatibility; this JSON budget adds function-count pressure so large files
# cannot grow new public API unnoticed.
BUDGET="scripts/architecture-budget.json"
if [ -f "$BUDGET" ]; then
  python3 - "$BUDGET" <<'PY'
import json
import pathlib
import re
import sys

budget_path = pathlib.Path(sys.argv[1])
budget = json.loads(budget_path.read_text())
failed = []
for raw_path, limits in budget.items():
    path = pathlib.Path(raw_path)
    if not path.exists():
        failed.append(f"{raw_path}: missing")
        continue
    text = path.read_text(errors="replace")
    lines = text.count("\n") + (0 if text.endswith("\n") else 1)
    public_fns = len(re.findall(r"(?m)^\s*pub\s+fn\s+[A-Za-z_][A-Za-z0-9_]*", text))
    public_fn_names = re.findall(r"(?m)^\s*pub\s+fn\s+([A-Za-z_][A-Za-z0-9_]*)", text)
    max_lines = limits.get("max_lines")
    max_public_fns = limits.get("max_public_fns")
    allowed_public_fns = limits.get("allowed_public_fns")
    if max_lines is not None and lines > max_lines:
        failed.append(f"{raw_path}: {lines} lines > max_lines {max_lines}")
    if max_public_fns is not None and public_fns > max_public_fns:
        failed.append(f"{raw_path}: {public_fns} public fns > max_public_fns {max_public_fns}")
    if allowed_public_fns is not None:
        allowed = set(allowed_public_fns)
        actual = set(public_fn_names)
        unexpected = sorted(actual - allowed)
        missing = sorted(allowed - actual)
        if unexpected:
            failed.append(f"{raw_path}: unexpected public fns {unexpected}")
        if missing:
            failed.append(f"{raw_path}: allowed public fns missing {missing}")

if failed:
    for item in failed:
        print(f"ARCH-GUARD FAIL: {item}", file=sys.stderr)
    sys.exit(1)
print(f"ARCH-GUARD ok:  {budget_path} budgets satisfied")
PY
else
  fail "missing $BUDGET"
fi

# 1. Root monolith line ceiling (zero growth; reductions always pass).
lib_lines="$(wc -l < src/lib.rs | tr -d '[:space:]')"
if [ "$lib_lines" -gt "$SRC_LIB_RS_MAX_LINES" ]; then
  fail "src/lib.rs has $lib_lines lines, cap is $SRC_LIB_RS_MAX_LINES. Move new domain code into a crates/opensks-* crate instead of growing the root monolith (docs/architecture-ownership.md). To intentionally raise the cap, lower it is free; raising needs docs/architecture-exceptions/."
fi
ok "src/lib.rs $lib_lines <= $SRC_LIB_RS_MAX_LINES lines"

# 2. main.rs must remain a thin entry shim.
main_lines="$(wc -l < src/main.rs | tr -d '[:space:]')"
if [ "$main_lines" -gt "$MAIN_RS_MAX_LINES" ]; then
  fail "src/main.rs has $main_lines lines, cap is $MAIN_RS_MAX_LINES. main.rs must stay a thin entry shim that delegates to opensks::*."
fi
ok "src/main.rs $main_lines <= $MAIN_RS_MAX_LINES lines"

# 3. No new inline domain modules in the root monolith (only `mod tests`).
extra_mods="$(grep -nE '^[[:space:]]*(pub[[:space:]]+)?mod[[:space:]]+[A-Za-z_][A-Za-z0-9_]*' src/lib.rs | grep -vE 'mod[[:space:]]+tests([[:space:]]|\{|;)' || true)"
if [ -n "$extra_mods" ]; then
  fail "src/lib.rs declares a non-test module; a new subsystem must live in a crates/opensks-* crate:
$extra_mods"
fi
ok "src/lib.rs declares no non-test modules"

# 4. Data-plane manifest must exist (shared/local boundary contract).
[ -f .opensks/data-plane-manifest.json ] || fail ".opensks/data-plane-manifest.json is missing (shared/local data-plane contract)."
ok ".opensks/data-plane-manifest.json present"

# 5. No broad '.opensks/' ignore (it would hide shared durable records).
if grep -nE '^\.opensks/$' .gitignore >/dev/null 2>&1; then
  fail ".gitignore contains a broad '.opensks/' rule; shared durable records (wiki/architecture/glossary/history summaries/design-systems) must stay trackable. Use specific '.opensks/<subdir>/' rules."
fi
ok ".gitignore has no broad '.opensks/' rule"

# 6. No developer-machine absolute paths or forbidden PRD source markers in
#    tracked sources/docs. Patterns are assembled by concatenation so this guard
#    never matches its own literals.
dev_path="/Users/""weklem"
prd_marker="$(printf '%s' 'P''RD_SOURCE_PATH')"
prd_slug="opensks_""prd_v3"
if grep -RIlE "$dev_path|$prd_marker|$prd_slug" Cargo.toml src docs README.md .github crates schemas 2>/dev/null; then
  fail "Found a developer-machine path or forbidden PRD marker in the tracked files listed above."
fi
ok "no developer-machine paths / forbidden PRD markers in tracked sources"

# 7. Recovery-release invariants (directive Appendix C): the chat-first runtime
#    corrections must not regress. Each guards a landed P0 fix.

# 7a. RootView must not instantiate the removed right-hand ComposerView. The
#     leading [^A-Za-z] boundary avoids matching CommitComposerView / Conversation*.
if grep -nE '(^|[^A-Za-z])ComposerView\(' swift/Sources/RootView.swift >/dev/null 2>&1; then
  fail "RootView instantiates ComposerView; the permanent right composer is removed — Chat's composer is the primary control (SHELL-001 / §0.3)."
fi
ok "RootView does not instantiate ComposerView"

# 7b. The product conversation path must drive a real AgentAdapter, never the
#     engine's deterministic template dispatcher. The pattern is assembled by
#     concatenation so this guard never matches its own literal. (The standalone
#     `graph`/`scheduler` smoke-test commands are a separate surface and may
#     still reference the deterministic worker / template graph names.)
conv_template="run_template""_with_event_stream"
if grep -rnE "$conv_template" crates/opensks-cli/src >/dev/null 2>&1; then
  fail "crates/opensks-cli/src calls the engine deterministic template dispatcher; conversation turns must use a real adapter (CHAT-001)."
fi
ok "conversation CLI path uses no deterministic template dispatcher"

# 7b2. Swift Chat's live turn-start must use the typed daemon request, not the
#      synchronous compatibility CLI `conversation turn-start` path (P0-001).
swift_turn_start_cli='"conversation",[[:space:]]*"turn-start"'
if grep -nE "$swift_turn_start_cli" swift/Sources/Conversations/ConversationService.swift >/dev/null 2>&1; then
  fail "ConversationService.swift shells the compatibility 'conversation turn-start' CLI path; Chat turn-start must use daemon conversation_turn_start."
fi
ok "Swift Chat turn-start uses daemon conversation_turn_start, not CLI compatibility path"

# 7c. Chat is the default workspace route (Chat is the main workspace).
if ! grep -qE 'route:[[:space:]]*WorkspaceRoute[[:space:]]*=[[:space:]]*\.chat' swift/Sources/Navigation/NavigationStore.swift; then
  fail "NavigationStore default route is not .chat (SHELL-002 / §3.3)."
fi
ok "NavigationStore defaults to .chat"

# 7d. Navigation has a single source of truth — no legacy selectedRail state.
if grep -qE 'var[[:space:]]+selectedRail' swift/Sources/Backend.swift; then
  fail "AppState.selectedRail reintroduced; navigation must have one source of truth, NavigationStore.route (SHELL-003 / NAV-102)."
fi
ok "no AppState.selectedRail dual navigation state"

# 7e. Daemon responses complete on an EXPLICIT terminal marker, never on a silence
#     / quiet-window heuristic (STREAM-001 / §0.4). The forbidden pattern is a
#     completion that fires after N seconds of stdout silence.
if grep -qE 'quietWindow' swift/Sources/Backend.swift; then
  fail "Backend.swift reintroduced a quiet-window completion heuristic; daemon responses must complete on the explicit request_completed terminal marker (STREAM-001 / §0.4)."
fi
ok "no quiet-window stream-completion heuristic in Backend.swift"

# 7e2. Swift domain services must not instantiate child processes directly.
#      Temporary allowlist: Backend.swift still owns the legacy CLIRunner plus
#      the daemon session bootstrap; ProcessSupervisor.swift is the single shared
#      process launcher. New domain service process creation must route through
#      ProcessSupervisor or, ultimately, the daemon client.
python3 - <<'PY'
import pathlib
import re
import sys

allowed = {
    pathlib.Path("swift/Sources/Backend.swift"),
    pathlib.Path("swift/Sources/Runtime/ProcessSupervisor.swift"),
}
failures = []
for path in pathlib.Path("swift/Sources").rglob("*.swift"):
    text = path.read_text(errors="replace")
    if re.search(r"(?<![A-Za-z0-9_])Process\s*\(", text) and path not in allowed:
        failures.append(str(path))

if failures:
    for failure in failures:
        print(
            f"ARCH-GUARD FAIL: {failure} instantiates Process(); use ProcessSupervisor or daemon client",
            file=sys.stderr,
        )
    sys.exit(1)
PY
ok "Swift domain services do not instantiate Process directly"

# 7f. Provider dispatch must not depend on a system curl subprocess.
if grep -rnE 'Command::new\("curl"\)|curl stdin|CurlChatCompleter' crates/opensks-adapter/src >/dev/null 2>&1; then
  fail "OpenRouter provider transport reintroduced curl subprocess usage; use native HTTP transport."
fi
ok "OpenRouter provider transport has no curl subprocess"

# 7g. Adapter product code must not own ad-hoc content identity or direct writes.
python3 - <<'PY'
import pathlib
import sys

failures = []
for path in pathlib.Path("crates/opensks-adapter/src").rglob("*.rs"):
    text = path.read_text(errors="replace")
    product = text.split("\n#[cfg(test)]", 1)[0]
    if "DefaultHasher" in product:
        failures.append(f"{path}: DefaultHasher content identity is forbidden")
    if "std::fs::write" in product:
        failures.append(f"{path}: product std::fs::write must go through opensks-patch-engine")

if failures:
    for failure in failures:
        print(f"ARCH-GUARD FAIL: {failure}", file=sys.stderr)
    sys.exit(1)
PY
ok "adapter content identity/write path is patch-engine owned"

echo "ARCH-GUARD: all checks passed."
