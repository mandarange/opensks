# Release Readiness Baseline — 07eda8a

This baseline is not a public-commercial release approval. It records the
required checks and known stop-ship gaps that must be closed before a release
candidate can be tagged.

## Required Status Checks

- `ci-core / core`
- `ci-integration / integration`
- `ci-security / security`
- `ci-macos-app / macos-app`
- `ci-performance / performance` for scheduled/performance release branches
- `codeql / codeql (rust)`
- `codeql / codeql (swift)`

Missing statuses block release even when local commands pass.

## Required Release Artifacts

- Core evidence: cargo metadata, locked cargo tree, workspace test log.
- Security evidence: cargo-audit log, CycloneDX SBOM JSON files, gitleaks JSON
  report, OpenSKS security audit log.
- macOS evidence: daemon bridge NDJSON smoke logs and bundled app smoke output.
- Runtime truth: regenerated schemas and runtime capability matrix from the same
  commit SHA.

## Known Stop-Ship Gaps

- Protocol v2 framed transport is still not implemented.
- Provider registry and keychain-backed secret resolution are still partial.
- PatchEngine has a safe apply foundation, but isolated worktree conflict
  arbitration and full verification gating remain incomplete.
- Root `src/lib.rs` has been reduced from 20,239 to 14,870 lines by moving the
  inline test module to `src/tests.rs` and moving the PRD coverage ledger to
  `assets/prd-requirements.tsv`; it is below the P1 15k ratchet but remains
  above later PR-062/PR-065 reduction targets.
- Release proof now records required release artifact SHA-256 digests, source
  commit SHA, tracked-worktree dirtiness, missing artifacts, blockers, and a
  same-SHA artifact binding gate. A public release still requires clean CI
  evidence plus signing, notarization, upgrade, sanitizer, and E2E gates.
