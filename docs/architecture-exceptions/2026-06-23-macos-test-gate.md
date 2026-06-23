# Architecture exception: macOS gate on the app-bundle test (+2 lines, src/lib.rs)

**Date:** 2026-06-23
**Cap change:** `SRC_LIB_RS_MAX_LINES` 20237 → 20239 (+2)
**Approved scope:** a test-only `#[cfg]` gate — no domain code, and it REDUCES what
compiles on Linux.

## Why

`tests::empty_args_creates_native_app_bundle` exercises the empty-args default,
which builds a SwiftUI `.app` via `compile_swift_app`. That function returns an
error off macOS ("the SwiftUI app can only be built on macOS"), so on the
`ubuntu-latest` `ci-core` job the test's `.expect("empty launch")` panicked. The
failure had been masked by an unrelated clippy failure that aborted the job before
the test step ran; once clippy was fixed, the test step ran and surfaced it.

## What was added to src/lib.rs (+2 lines)

1. `#[cfg(target_os = "macos")]` on the test (1 line).
2. A 1-line comment explaining the gate (1 line).

This matches the file's existing macOS `#[cfg(target_os = "macos")]` gating of the
Swift helpers (`compile_swift_app`, `find_swift_package_dir`, …). The macOS
`ci-macos-app` job still runs the test; the other 73 root-crate tests are
platform-neutral and unaffected.

## Note

The cap counts raw `src/lib.rs` lines, including `#[cfg(test)]`. This is a test
gate, not monolith domain growth — it makes LESS code compile on Linux. When the
root crate's tests are extracted, this gate moves with them and the cap can be
lowered again (lowering needs no exception).
