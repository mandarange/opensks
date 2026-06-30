#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/commercial-readiness-check.sh

Run the current OpenSKS local-commercial readiness gates and write a concise
machine/human report under .opensks/qa/.

The check intentionally does not read or print provider secret values. It only
uses redacted receipts produced by the OpenSKS CLI.
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: commercial readiness check currently expects macOS tooling." >&2
  exit 1
fi

for tool in date plutil; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    exit 1
  fi
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"

cli="${OPENSKS_CLI:-target/debug/opensks}"
if [[ ! -x "$cli" ]]; then
  echo "==> Building OpenSKS CLI for readiness checks"
  cargo build --quiet
fi
if [[ ! -x "$cli" ]]; then
  echo "error: OpenSKS CLI not executable at $cli" >&2
  exit 1
fi

mkdir -p .opensks/qa
json_report=".opensks/qa/commercial-readiness.json"
md_report=".opensks/qa/commercial-readiness.md"
provider_log=".opensks/qa/commercial-readiness-provider.log"
release_log=".opensks/qa/commercial-readiness-release.log"
acceptance_log=".opensks/qa/commercial-readiness-acceptance.log"
capability_report=".opensks/qa/commercial-readiness-capability-report.json"

json_escape() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/}"
  printf '%s' "$value"
}

json_string() {
  printf '"%s"' "$(json_escape "$1")"
}

json_bool() {
  if [[ "$1" == "true" ]]; then
    printf 'true'
  else
    printf 'false'
  fi
}

extract_json_raw() {
  local path="$1"
  local key="$2"
  local fallback="${3:-}"
  if [[ -f "$path" ]]; then
    plutil -extract "$key" raw -o - "$path" 2>/dev/null || printf '%s' "$fallback"
  else
    printf '%s' "$fallback"
  fi
}

append_json_string_array_from_plutil() {
  local path="$1"
  local key="$2"
  if [[ ! -f "$path" ]]; then
    printf '[]'
    return
  fi
  local count
  count="$(plutil -extract "$key" raw -o - "$path" 2>/dev/null || printf '0')"
  if ! [[ "$count" =~ ^[0-9]+$ ]]; then
    printf '[]'
    return
  fi
  printf '['
  local index
  for ((index = 0; index < count; index++)); do
    if [[ "$index" -gt 0 ]]; then
      printf ','
    fi
    json_string "$(extract_json_raw "$path" "$key.$index" "")"
  done
  printf ']'
}

append_release_blockers() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    printf '[]'
    return
  fi
  local count
  count="$(plutil -extract blockers raw -o - "$path" 2>/dev/null || printf '0')"
  if ! [[ "$count" =~ ^[0-9]+$ ]]; then
    printf '[]'
    return
  fi
  printf '['
  local index code message
  for ((index = 0; index < count; index++)); do
    if [[ "$index" -gt 0 ]]; then
      printf ','
    fi
    code="$(extract_json_raw "$path" "blockers.$index.code" "unknown")"
    message="$(extract_json_raw "$path" "blockers.$index.message" "")"
    printf '{"code":'
    json_string "$code"
    printf ',"message":'
    json_string "$message"
    printf '}'
  done
  printf ']'
}

append_bash_string_array() {
  local item
  printf '['
  local first=true
  for item in "$@"; do
    if [[ "$first" == "true" ]]; then
      first=false
    else
      printf ','
    fi
    json_string "$item"
  done
  printf ']'
}

append_coding_blockers() {
  set +u
  local count="${#coding_blockers[@]}"
  set -u
  if [[ "$count" == "0" ]]; then
    printf '[]'
    return
  fi
  append_bash_string_array "${coding_blockers[@]}"
}

coding_blocker_count() {
  set +u
  local count="${#coding_blockers[@]}"
  set -u
  printf '%s' "$count"
}

capability_field() {
  local id="$1"
  local field="$2"
  local fallback="${3:-}"
  if [[ ! -f "$capability_report" ]]; then
    printf '%s' "$fallback"
    return
  fi
  local count
  count="$(plutil -extract capabilities raw -o - "$capability_report" 2>/dev/null || printf '0')"
  if ! [[ "$count" =~ ^[0-9]+$ ]]; then
    printf '%s' "$fallback"
    return
  fi
  local index current
  for ((index = 0; index < count; index++)); do
    current="$(plutil -extract "capabilities.$index.id" raw -o - "$capability_report" 2>/dev/null || true)"
    if [[ "$current" == "$id" ]]; then
      plutil -extract "capabilities.$index.$field" raw -o - "$capability_report" 2>/dev/null || printf '%s' "$fallback"
      return
    fi
  done
  printf '%s' "$fallback"
}

echo "==> Checking runtime capability report"
"$cli" capability report >"$capability_report" 2>.opensks/qa/commercial-readiness-capability.log || true

echo "==> Checking provider adapter readiness"
"$cli" provider adapter-check >"$provider_log" 2>&1 || true

echo "==> Checking release proof"
"$cli" release proof >"$release_log" 2>&1 || true

echo "==> Checking acceptance audit"
"$cli" acceptance audit >"$acceptance_log" 2>&1 || true

install_receipt=".opensks/macos/install-smoke-receipt.json"
provider_receipt=".opensks/providers/provider-adapter-check.json"
release_receipt=".opensks/release/release-proof.json"

archive_verified="$(extract_json_raw "$install_receipt" archive_verified false)"
archive_relocatable="$(extract_json_raw "$install_receipt" archive_relocatable false)"
archive_provider_docs="$(extract_json_raw "$install_receipt" archive_provider_setup_documented false)"
archive_release_docs="$(extract_json_raw "$install_receipt" archive_release_limits_documented false)"
archive_install_passed=false
if [[ "$archive_verified" == "true" && "$archive_relocatable" == "true" && "$archive_provider_docs" == "true" && "$archive_release_docs" == "true" ]]; then
  archive_install_passed=true
fi

provider_attempted="$(extract_json_raw "$provider_receipt" summary.attempted 0)"
provider_reachable="$(extract_json_raw "$provider_receipt" summary.reachable 0)"
provider_blocker_count="$(extract_json_raw "$provider_receipt" blockers 0)"
provider_secret_exposed="$(extract_json_raw "$provider_receipt" secret_value_exposed false)"
provider_passed=false
if [[ "$provider_blocker_count" == "0" && "$provider_secret_exposed" == "false" && "$provider_reachable" == "2" ]]; then
  provider_passed=true
fi

release_status="$(extract_json_raw "$release_receipt" status missing)"
release_blocker_count="$(extract_json_raw "$release_receipt" blockers 0)"
release_passed=false
if [[ "$release_status" == "verified" && "$release_blocker_count" == "0" ]]; then
  release_passed=true
fi

acceptance_passed="$(sed -n 's/^passed: //p' "$acceptance_log" | tail -1)"
acceptance_partial="$(sed -n 's/^partial: //p' "$acceptance_log" | tail -1)"
acceptance_failed="$(sed -n 's/^failed: //p' "$acceptance_log" | tail -1)"
acceptance_total="$(sed -n 's/^criteria: //p' "$acceptance_log" | tail -1)"
acceptance_passed="${acceptance_passed:-0}"
acceptance_partial="${acceptance_partial:-0}"
acceptance_failed="${acceptance_failed:-1}"
acceptance_total="${acceptance_total:-0}"
acceptance_gate_passed=false
if [[ "$acceptance_failed" == "0" && "$acceptance_partial" == "0" ]]; then
  acceptance_gate_passed=true
fi

model_dispatch_available="$(capability_field model.dispatch available false)"
model_dispatch_reason="$(capability_field model.dispatch reason_code missing_capability_report)"
agent_code_edit_available="$(capability_field agent.code_edit available false)"
agent_code_edit_reason="$(capability_field agent.code_edit reason_code missing_capability_report)"
agent_parallel_build_available="$(capability_field agent.parallel_build available false)"
agent_parallel_build_reason="$(capability_field agent.parallel_build reason_code missing_capability_report)"
local_test_edit_available="$(capability_field agent.local_test_edit available false)"
local_test_edit_reason="$(capability_field agent.local_test_edit reason_code missing_capability_report)"

coding_blockers=()
if [[ "$model_dispatch_available" != "true" ]]; then
  coding_blockers+=("model_dispatch_unverified:$model_dispatch_reason")
fi
if [[ "$agent_code_edit_available" != "true" ]]; then
  coding_blockers+=("agent_code_edit_unverified:$agent_code_edit_reason")
fi
if [[ "$agent_parallel_build_available" != "true" ]]; then
  coding_blockers+=("agent_parallel_build_unverified:$agent_parallel_build_reason")
fi
if [[ "$local_test_edit_available" != "true" && "$agent_code_edit_available" != "true" ]]; then
  coding_blockers+=("release_local_test_fallback_unavailable:$local_test_edit_reason")
fi

coding_execution_passed=false
if [[ "$model_dispatch_available" == "true" && "$agent_code_edit_available" == "true" && "$agent_parallel_build_available" == "true" ]]; then
  coding_execution_passed=true
fi

ready=false
if [[ "$archive_install_passed" == "true" && "$provider_passed" == "true" && "$release_passed" == "true" && "$acceptance_gate_passed" == "true" && "$coding_execution_passed" == "true" ]]; then
  ready=true
fi

generated_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

{
  printf '{\n'
  printf '  "schema": "opensks.commercial-readiness.v1",\n'
  printf '  "generated_at": %s,\n' "$(json_string "$generated_at")"
  printf '  "repo_root": %s,\n' "$(json_string "$repo_root")"
  printf '  "ready": %s,\n' "$(json_bool "$ready")"
  printf '  "install_archive": {\n'
  printf '    "passed": %s,\n' "$(json_bool "$archive_install_passed")"
  printf '    "receipt": %s,\n' "$(json_string "$install_receipt")"
  printf '    "archive_verified": %s,\n' "$(json_bool "$archive_verified")"
  printf '    "archive_relocatable": %s,\n' "$(json_bool "$archive_relocatable")"
  printf '    "provider_setup_documented": %s,\n' "$(json_bool "$archive_provider_docs")"
  printf '    "release_limits_documented": %s\n' "$(json_bool "$archive_release_docs")"
  printf '  },\n'
  printf '  "provider_adapters": {\n'
  printf '    "passed": %s,\n' "$(json_bool "$provider_passed")"
  printf '    "receipt": %s,\n' "$(json_string "$provider_receipt")"
  printf '    "attempted": %s,\n' "$provider_attempted"
  printf '    "reachable": %s,\n' "$provider_reachable"
  printf '    "secret_value_exposed": %s,\n' "$(json_bool "$provider_secret_exposed")"
  printf '    "blockers": '
  append_json_string_array_from_plutil "$provider_receipt" blockers
  printf '\n  },\n'
  printf '  "coding_execution": {\n'
  printf '    "passed": %s,\n' "$(json_bool "$coding_execution_passed")"
  printf '    "capability_report": %s,\n' "$(json_string "$capability_report")"
  printf '    "model_dispatch_available": %s,\n' "$(json_bool "$model_dispatch_available")"
  printf '    "model_dispatch_reason": %s,\n' "$(json_string "$model_dispatch_reason")"
  printf '    "agent_code_edit_available": %s,\n' "$(json_bool "$agent_code_edit_available")"
  printf '    "agent_code_edit_reason": %s,\n' "$(json_string "$agent_code_edit_reason")"
  printf '    "agent_parallel_build_available": %s,\n' "$(json_bool "$agent_parallel_build_available")"
  printf '    "agent_parallel_build_reason": %s,\n' "$(json_string "$agent_parallel_build_reason")"
  printf '    "local_test_edit_available": %s,\n' "$(json_bool "$local_test_edit_available")"
  printf '    "local_test_edit_reason": %s,\n' "$(json_string "$local_test_edit_reason")"
  printf '    "blockers": '
  append_coding_blockers
  printf '\n  },\n'
  printf '  "release_proof": {\n'
  printf '    "passed": %s,\n' "$(json_bool "$release_passed")"
  printf '    "receipt": %s,\n' "$(json_string "$release_receipt")"
  printf '    "status": %s,\n' "$(json_string "$release_status")"
  printf '    "blockers": '
  append_release_blockers "$release_receipt"
  printf '\n  },\n'
  printf '  "acceptance": {\n'
  printf '    "passed": %s,\n' "$(json_bool "$acceptance_gate_passed")"
  printf '    "total": %s,\n' "$acceptance_total"
  printf '    "passed_count": %s,\n' "$acceptance_passed"
  printf '    "partial_count": %s,\n' "$acceptance_partial"
  printf '    "failed_count": %s,\n' "$acceptance_failed"
  printf '    "log": %s\n' "$(json_string "$acceptance_log")"
  printf '  }\n'
  printf '}\n'
} > "$json_report"

plutil -convert json -o - "$json_report" >/dev/null

{
  printf '# OpenSKS Commercial Readiness\n\n'
  printf -- '- Generated: `%s`\n' "$generated_at"
  printf -- '- Ready for commercial use: `%s`\n\n' "$ready"
  printf '## Gates\n\n'
  printf -- '- Install archive: `%s` (archive verified `%s`, relocatable `%s`)\n' "$archive_install_passed" "$archive_verified" "$archive_relocatable"
  printf -- '- Provider adapters: `%s` (attempted `%s`, reachable `%s`, blockers `%s`, secret exposed `%s`)\n' "$provider_passed" "$provider_attempted" "$provider_reachable" "$provider_blocker_count" "$provider_secret_exposed"
  printf -- '- Coding execution: `%s` (dispatch `%s`, code edit `%s`, parallel build `%s`, blockers `%s`)\n' "$coding_execution_passed" "$model_dispatch_available" "$agent_code_edit_available" "$agent_parallel_build_available" "$(coding_blocker_count)"
  printf -- '- Release proof: `%s` (status `%s`, blockers `%s`)\n' "$release_passed" "$release_status" "$release_blocker_count"
  printf -- '- Acceptance: `%s` (`%s` total / `%s` passed / `%s` partial / `%s` failed)\n\n' "$acceptance_gate_passed" "$acceptance_total" "$acceptance_passed" "$acceptance_partial" "$acceptance_failed"
  printf '## Artifacts\n\n'
  printf -- '- JSON report: `%s`\n' "$json_report"
  printf -- '- Install receipt: `%s`\n' "$install_receipt"
  printf -- '- Provider receipt: `%s`\n' "$provider_receipt"
  printf -- '- Capability report: `%s`\n' "$capability_report"
  printf -- '- Release proof: `%s`\n' "$release_receipt"
  printf -- '- Acceptance log: `%s`\n\n' "$acceptance_log"
  if [[ "$ready" != "true" ]]; then
    printf '## Remaining Blockers\n\n'
    if [[ "$provider_passed" != "true" ]]; then
      printf -- '- Provider live readiness is not proven. Run with `OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1` and configured OpenRouter/OpenAI credentials.\n'
    fi
    if [[ "$release_passed" != "true" ]]; then
      printf -- '- Release proof is not verified. Resolve dirty workspace, production Developer ID signing, and Apple notarization blockers.\n'
    fi
    if [[ "$coding_execution_passed" != "true" ]]; then
      printf -- '- Real provider-backed coding execution is not proven. `agent.code_edit`, `agent.parallel_build`, and `model.dispatch` must be available in the runtime capability report; release builds cannot rely on the developer-only local-test simulation adapter.\n'
    fi
    if [[ "$acceptance_gate_passed" != "true" ]]; then
      printf -- '- Acceptance still has partial or failed criteria.\n'
    fi
    if [[ "$archive_install_passed" != "true" ]]; then
      printf -- '- Local macOS archive install smoke must pass with archive relocation checks.\n'
    fi
  fi
} > "$md_report"

echo "OpenSKS commercial readiness: $ready"
echo "  install_archive: $archive_install_passed"
echo "  provider_adapters: $provider_passed"
echo "  coding_execution: $coding_execution_passed"
echo "  release_proof: $release_passed"
echo "  acceptance: $acceptance_gate_passed"
echo "  report: $json_report"
echo "  summary: $md_report"
