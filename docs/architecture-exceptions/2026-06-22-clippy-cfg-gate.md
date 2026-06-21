# Exception: raise `SRC_LIB_RS_MAX_LINES` 20102 → 20108

- **Date:** 2026-06-22
- **Rule relaxed:** `scripts/architecture-ownership.config` `SRC_LIB_RS_MAX_LINES`
  (the `src/lib.rs` line cap), raised from **20102** to **20108** (+6 lines).
- **Approver:** cdw0424 (repo owner).

## Why

CI's `ci-core` job runs on `ubuntu-latest` and was failing at the Clippy step
(`-D warnings` → `-D dead-code`) on five items in `src/lib.rs`:
`SWIFT_PACKAGE_DIR_ENV`, `SWIFT_STUDIO_PRODUCT`, `swift_package_dir_from_root`,
`swift_package_dir_from_ancestors`, `find_swift_package_dir`. These are used
**only** by the `#[cfg(target_os = "macos")]` build of `compile_swift_app`, so on
Linux they are genuinely dead code. (They are live on macOS, which is why local
clippy passed — this is a platform-cfg bug, not a toolchain-version issue, so
pinning the toolchain would not fix it.)

The proper fix is to `#[cfg(target_os = "macos")]`-gate the five items and their
one unit test so they are compiled only where they are used. That adds **six
`#[cfg(...)]` attribute lines** to `src/lib.rs` — annotations on existing code,
**not** new domain logic — which pushes the file from 20102 to 20108 lines and
would otherwise trip the zero-growth cap set in PR-020.

The cap is raised by exactly the six annotation lines. No new module is added;
the guard's "no non-test module" rule still holds.

## Removal / retirement

This +6 allowance retires when the macOS app-bundling helpers
(`compile_swift_app`, `create_native_app_bundle`, `find_swift_package_dir`, and
the `SWIFT_*` constants) migrate out of `src/lib.rs` into a dedicated
`crates/opensks-*` (or `xtask`) bundling crate as part of the monolith-reduction
milestones in [../architecture-ownership.md](../architecture-ownership.md). At
that point `src/lib.rs` shrinks well below 20102 and the cap should be lowered
again.
