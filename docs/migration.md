# OpenSKS Migration Guide

This guide covers migrating an existing OpenSKS workspace to the conversation-first
studio. It mirrors the migration strategy in the engineering directive (§19). All
migrations are **non-destructive**: the old database and data are never deleted, and
no migration step pushes or writes anything externally.

## 19.1 Database migration

- Each schema carries an explicit **schema version**.
- Before a major migration the database is **backed up to a local, git-ignored
  path** (under `.opensks/runtime/`), never to a tracked or shared location.
- Migrations run **transactionally** — a failed migration rolls back to the
  pre-migration state.
- On startup the engine **validates integrity**; a failed validation enters a
  **read-only recovery mode** and **does not delete** the old database.
- Inspect and recover with:
  - `opensks doctor database` — integrity diagnostics.
  - `opensks migrate status` — current schema version and pending migrations.

## 19.2 Existing app data

- Legacy mission/run summaries are imported as **historical runs** where a stable
  identity is available; existing **evidence refs are preserved**.
- Conversation text is **never invented from logs**.
- Imported history is surfaced as an **`Imported history` conversation** that
  contains **safe summary cards only** (no raw logs, no secrets).

## 19.3 Existing file tabs

- Open paths are converted to **canonical workspace-relative document records**.
- **Invalid, missing, or secret paths are not restored**; the workspace shows a
  truthful recovery notice instead of a blank or fabricated tab.
- Old random tab IDs are **not** treated as durable identity.

## 19.4 Existing theme

- Old color/style names are mapped to **semantic design tokens** (dark-only).
- Deprecation is logged **only in developer diagnostics**, not surfaced to users.
- Token aliases are removed **after** all source is migrated and an audit shows no
  hard-coded color usage remains.

## 19.5 Existing graph templates

- Position/layout metadata is added **without changing execution semantics**.
- Old graphs get a **deterministic initial layout**.
- Graph **content-hash semantics are preserved**; view metadata is versioned
  separately where necessary so layout changes never alter a graph's identity.

## 19.6 Existing `.gitignore`

- The OpenSKS managed-block migration is **idempotent** and only edits content
  **between its markers** — user rules outside the markers are never removed.
- A migration **surfaces conflicts** where a broad user rule would hide a shared
  design / history / wiki path that must stay trackable.
- A **dry run** is available before any change is written.

## 19.7 Rollback

Every PR that changes storage or protocol ships with:

- a **backward read path** or an explicit minimum supported version,
- a **feature flag** where needed,
- a **migration rollback / backup** procedure,
- a **compatibility UI fallback**, and
- **proof that rollback performs no external push or write** (verified by the
  release proof and the security audit gate).
