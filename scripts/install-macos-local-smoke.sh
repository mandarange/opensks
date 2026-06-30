#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install-macos-local-smoke.sh [--clean] [--archive] [--open]

Build and verify the local OpenSKS macOS app bundle from this checkout.

Options:
  --clean    Remove the generated .opensks/macos bundle before building.
  --archive  Create and verify a local .opensks/macos/OpenSKS-local-macos.zip.
  --open     Open the generated OpenSKS.app after verification.
  -h, --help
            Show this help.
EOF
}

clean=0
create_archive=0
open_app=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --clean)
      clean=1
      ;;
    --archive)
      create_archive=1
      ;;
    --open)
      open_app=1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: OpenSKS local app smoke requires macOS." >&2
  exit 1
fi

for tool in cargo swift codesign plutil; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: required tool not found: $tool" >&2
    echo "hint: install Rust and Xcode Command Line Tools, then rerun this script." >&2
    exit 1
  fi
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repo_root"

app_path=".opensks/macos/OpenSKS.app"
app_binary="$app_path/Contents/MacOS/OpenSKS"
cli_resource="$app_path/Contents/Resources/opensks-cli"
workspace_file="$app_path/Contents/Resources/workspace-path.txt"
receipt_path=".opensks/macos/install-smoke-receipt.json"
archive_path=".opensks/macos/OpenSKS-local-macos.zip"
archive_stage=".opensks/macos/OpenSKS-local-macos"
archive_launcher="$archive_stage/OpenSKS-Launch.command"
archive_install_notes="$archive_stage/INSTALL.txt"

if [[ "$clean" -eq 1 ]]; then
  rm -rf .opensks/macos
fi

echo "==> Checking CLI entrypoint"
cargo run -- --help >/tmp/opensks-install-smoke-help.txt

echo "==> Running terminal smoke"
cargo run -- terminal smoke

echo "==> Building local macOS app bundle"
OPENSKS_SKIP_DASHBOARD_OPEN=1 cargo run --quiet

echo "==> Verifying generated bundle"
test -x "$app_binary"
test -x "$cli_resource"
test -f "$workspace_file"

if command -v file >/dev/null 2>&1; then
  file "$app_binary" | grep -q 'Mach-O'
fi

recorded_workspace="$(tr -d '\r\n' < "$workspace_file")"
if [[ "$recorded_workspace" != "$repo_root" ]]; then
  echo "error: workspace-path.txt does not match repo root" >&2
  echo "expected: $repo_root" >&2
  echo "actual:   $recorded_workspace" >&2
  exit 1
fi

codesign --verify --deep --strict "$app_path"

archive_created=false
archive_verified=false
archive_mach_o_verified=false
archive_codesign_verified=false
archive_recorded_workspace_matches=false
archive_workspace_override_smoke_verified=false
archive_launcher_verified=false
archive_metadata_clean=false
archive_install_notes_verified=false
archive_provider_setup_documented=false
archive_release_limits_documented=false
archive_abs=""
if [[ "$create_archive" -eq 1 ]]; then
  echo "==> Creating local macOS app archive"
  rm -f "$archive_path"
  rm -rf "$archive_stage"
  mkdir -p "$archive_stage"
  if command -v ditto >/dev/null 2>&1; then
    COPYFILE_DISABLE=1 ditto --norsrc --noextattr "$app_path" "$archive_stage/OpenSKS.app"
  else
    cp -R "$app_path" "$archive_stage/OpenSKS.app"
  fi
  find "$archive_stage" \( -name '._*' -o -name '.DS_Store' \) -delete
  cat > "$archive_launcher" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

bundle_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
workspace="${1:-$PWD}"
if [[ ! -d "$workspace" ]]; then
  echo "error: workspace does not exist: $workspace" >&2
  echo "usage: ./OpenSKS-Launch.command [workspace-directory]" >&2
  exit 2
fi
export OPENSKS_WORKSPACE="$(cd "$workspace" && pwd -P)"
exec "$bundle_dir/OpenSKS.app/Contents/MacOS/OpenSKS"
EOF
  chmod +x "$archive_launcher"
  cat > "$archive_install_notes" <<'EOF'
OpenSKS local macOS archive

1. Launch against the workspace you want OpenSKS to manage:

  ./OpenSKS-Launch.command /path/to/your/opensks/workspace

If no workspace path is passed, the launcher uses the current directory.
The launcher sets OPENSKS_WORKSPACE before starting OpenSKS.app so the app is
not tied to the workspace path baked into the machine that created the archive.

2. Configure live providers without writing secrets into this archive.

   Export credentials in the shell or add them through the app Provider Center
   before launching OpenSKS. For live OpenRouter/OpenAI adapter reachability
   proof, set:

     OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1
     OPENROUTER_API_KEY=...
     OPENAI_API_KEY=...

   Then run the Provider Center "Run provider check" action, or run:

     ./OpenSKS.app/Contents/Resources/opensks-cli provider adapter-check

   The report is written to .opensks/providers/provider-adapter-check.json and
   records blocker/action text without serializing secret values.

3. Verify local install and release posture from the target workspace.

   The generated local archive is ad-hoc signed for development use. Run:

     ./OpenSKS.app/Contents/Resources/opensks-cli release proof

   A fully verified production release still requires a clean source checkout,
   same-SHA artifact binding, Developer ID Application signing, and Apple notarization
   evidence. Missing external evidence is reported as blockers;
   do not treat an ad-hoc local archive as a notarized production binary.
EOF
  if command -v ditto >/dev/null 2>&1; then
    (cd .opensks/macos && COPYFILE_DISABLE=1 ditto --norsrc --noextattr -c -k --keepParent OpenSKS-local-macos OpenSKS-local-macos.zip)
  else
    (cd .opensks/macos && COPYFILE_DISABLE=1 zip -qry OpenSKS-local-macos.zip OpenSKS-local-macos)
  fi
  test -s "$archive_path"
  archive_listing="$(mktemp -t opensks-archive-listing.XXXXXX)"
  if command -v zipinfo >/dev/null 2>&1; then
    zipinfo -1 "$archive_path" > "$archive_listing"
  else
    unzip -Z -1 "$archive_path" > "$archive_listing"
  fi
  if grep -Eq '(^|/)\._|(^|/)\.DS_Store$' "$archive_listing"; then
    echo "error: archive contains macOS metadata sidecar files" >&2
    grep -E '(^|/)\._|(^|/)\.DS_Store$' "$archive_listing" >&2
    exit 1
  fi
  rm -f "$archive_listing"
  archive_metadata_clean=true

  echo "==> Verifying local archive extraction"
  extract_dir="$(mktemp -d -t opensks-install-smoke.XXXXXX)"
  trap 'rm -rf "$extract_dir"' EXIT
  if command -v ditto >/dev/null 2>&1; then
    ditto -x -k "$archive_path" "$extract_dir"
  else
    unzip -q "$archive_path" -d "$extract_dir"
  fi
  extracted_root="$extract_dir/OpenSKS-local-macos"
  extracted_app="$extracted_root/OpenSKS.app"
  extracted_launcher="$extracted_root/OpenSKS-Launch.command"
  extracted_binary="$extracted_app/Contents/MacOS/OpenSKS"
  extracted_cli="$extracted_app/Contents/Resources/opensks-cli"
  extracted_workspace_file="$extracted_app/Contents/Resources/workspace-path.txt"
  extracted_install_notes="$extracted_root/INSTALL.txt"
  test -x "$extracted_launcher"
  test -x "$extracted_binary"
  test -x "$extracted_cli"
  test -f "$extracted_install_notes"
  test -f "$extracted_workspace_file"
  grep -q 'OPENSKS_WORKSPACE' "$extracted_install_notes"
  grep -q 'OPENSKS_ALLOW_REMOTE_PROVIDER_PROBE=1' "$extracted_install_notes"
  grep -q 'provider adapter-check' "$extracted_install_notes"
  grep -q 'Developer ID Application' "$extracted_install_notes"
  grep -q 'Apple notarization' "$extracted_install_notes"
  archive_install_notes_verified=true
  archive_provider_setup_documented=true
  archive_release_limits_documented=true
  extracted_workspace="$(tr -d '\r\n' < "$extracted_workspace_file")"
  if [[ "$extracted_workspace" != "$repo_root" ]]; then
    echo "error: extracted workspace-path.txt does not match repo root" >&2
    echo "expected: $repo_root" >&2
    echo "actual:   $extracted_workspace" >&2
    exit 1
  fi
  test -d "$extracted_workspace"
  archive_recorded_workspace_matches=true
  bash -n "$extracted_launcher"
  grep -q 'OPENSKS_WORKSPACE' "$extracted_launcher"
  archive_launcher_verified=true
  relocated_workspace="$(mktemp -d -t opensks-relocated-workspace.XXXXXX)"
  mkdir -p "$relocated_workspace/.opensks"
  OPENSKS_WORKSPACE="$relocated_workspace" "$extracted_cli" --help >/tmp/opensks-relocated-cli-help.txt
  OPENSKS_WORKSPACE="$relocated_workspace" "$extracted_cli" terminal smoke >/tmp/opensks-relocated-terminal-smoke.txt
  test -d "$relocated_workspace/.opensks/runtime/terminal/sessions"
  rm -rf "$relocated_workspace"
  archive_workspace_override_smoke_verified=true
  if command -v file >/dev/null 2>&1; then
    file "$extracted_binary" | grep -q 'Mach-O'
    archive_mach_o_verified=true
  fi
  codesign --verify --deep --strict "$extracted_app"
  archive_codesign_verified=true
  archive_created=true
  archive_verified=true
  archive_abs="$repo_root/$archive_path"
fi

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

generated_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
cat > "$receipt_path" <<EOF
{
  "schema": "opensks.install-smoke-receipt.v1",
  "generated_at": $(json_string "$generated_at"),
  "repo_root": $(json_string "$repo_root"),
  "app_path": $(json_string "$repo_root/$app_path"),
  "app_binary_path": $(json_string "$repo_root/$app_binary"),
  "cli_resource_path": $(json_string "$repo_root/$cli_resource"),
  "workspace_file_path": $(json_string "$repo_root/$workspace_file"),
  "recorded_workspace": $(json_string "$recorded_workspace"),
  "recorded_workspace_matches": true,
  "cli_entrypoint_verified": true,
  "terminal_smoke_verified": true,
  "mach_o_verified": true,
  "codesign_verified": true,
  "network_install_performed": false,
  "archive_created": $archive_created,
  "archive_path": $(json_string "$archive_abs"),
  "archive_relocatable": $archive_workspace_override_smoke_verified,
  "archive_relocation_mode": "OpenSKS-Launch.command sets OPENSKS_WORKSPACE",
  "archive_workspace_path_embedded": true,
  "archive_recorded_workspace_matches": $archive_recorded_workspace_matches,
  "archive_launcher_verified": $archive_launcher_verified,
  "archive_install_notes_verified": $archive_install_notes_verified,
  "archive_provider_setup_documented": $archive_provider_setup_documented,
  "archive_release_limits_documented": $archive_release_limits_documented,
  "archive_metadata_clean": $archive_metadata_clean,
  "archive_workspace_override_smoke_verified": $archive_workspace_override_smoke_verified,
  "archive_mach_o_verified": $archive_mach_o_verified,
  "archive_codesign_verified": $archive_codesign_verified,
  "archive_verified": $archive_verified
}
EOF

echo "==> Validating install smoke receipt JSON"
plutil -convert json -o - "$receipt_path" >/dev/null

echo "OpenSKS local macOS smoke passed:"
echo "  app: $repo_root/$app_path"
echo "  cli: $repo_root/$cli_resource"
echo "  receipt: $repo_root/$receipt_path"
if [[ "$create_archive" -eq 1 ]]; then
  echo "  archive: $repo_root/$archive_path"
fi

if [[ "$open_app" -eq 1 ]]; then
  open "$app_path"
fi
