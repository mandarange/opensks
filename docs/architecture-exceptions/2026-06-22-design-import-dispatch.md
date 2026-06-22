# Architecture exception: `design import` subcommand dispatch (PR-039)

**Date:** 2026-06-22
**PR:** PR-039 — Design Import and Open Design Workflow
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20124 → 20136 (+12 lines)

## What

PR-039 adds the human-reviewed design-package quarantine pipeline. The `design`
verb already exists in root `src/lib.rs` (it owns `design qa`). This PR routes the
new `import` / `import-approve` / `import-reject` / `import-status` subcommands to
`opensks_cli::run_design_import_command`, which forwards to the
`opensks-design::import` quarantine pipeline. Root only gains a 9-line dispatch
guard at the top of `run_design_command`:

```rust
if let Some(sub) = args.first().map(String::as_str) {
    if matches!(sub, "import" | "import-approve" | "import-reject" | "import-status") {
        let output = opensks_cli::run_design_import_command(args, cwd).map_err(convert_cli_error)?;
        return Ok(CliOutput { stdout: output.stdout });
    }
}
```

## Why this is allowed

All security-critical data-plane logic (archive extraction, zip-slip / symlink /
executable-script / size-count / MIME defenses, provenance, atomic quarantine,
re-validate-before-promote, safe-delete-on-reject) lives in
`crates/opensks-design/src/import.rs`, and the CLI body lives in
`crates/opensks-cli` (`run_design_import_command`). Root keeps only the routing
shim, identical in spirit to the `file` and `conversation` verb dispatch. No new
domain module is added to `src/lib.rs`.
