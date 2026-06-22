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

echo "ARCH-GUARD: all checks passed."
