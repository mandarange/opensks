# OpenSKS Studio Dark — Design Guide

Agent- and human-readable design intent for the OpenSKS native Studio app. The
canonical machine-readable state lives in `tokens.json`; this document is the
prose source of truth those tokens reference.

## Visual Theme & Atmosphere

A calm, dark, elevated workspace. Hierarchy comes from a surface elevation
ladder and real materials, not borders on everything.

## Color Palette & Semantic Roles

Dark-only. One teal accent (`color.accent.primary`) carries brand/action/ready;
violet (`color.accent.secondary`) is reserved for the mark and keywords; gold and
coral are the honesty palette only (`color.status.warning`, `color.status.danger`).
Backgrounds ladder from `color.canvas` → `color.surface.sidebar` →
`color.surface.base` → `color.surface.raised`.

## Typography Rules

`color.text.primary` on `color.canvas` must meet a 7:1 contrast ratio (see the
token's `contrast_constraints`). Secondary and muted text step down legibly.

## Component Styling

Controls use `radius.control`; cards use `radius.card`. No decorative gradients
that reduce text contrast.

## Layout & Information Hierarchy

The labelled navigation rail is `size.rail.width` wide. Spacing follows the
`space.*` scale.

## Depth, Borders & Elevation

Seams use `color.border.subtle`; emphasized separations use `color.border.strong`.

## Interaction, Motion & Feedback

Primary hit targets are at least `size.hit_target.primary`; dense toolbar targets
at least `size.hit_target.toolbar`. Focus is shown with `color.focus`, never by
color alone. Respect reduced-motion.

## Accessibility & Responsive Behavior

The entire visible control tile is the hit target. Status is never communicated
by color alone.

## Agent Prompt Guide

When generating UI for this system: dark-only host chrome, conversation is the
primary surface, full-tile click targets, labelled rail, code editor fills
available width.

## Product-Specific Invariants

- Dark-only host chrome; no theme toggle.
- Canonical logo (not a synthetic mark) in title/welcome/about.
- Code editor uses full available width.

## Forbidden Patterns

- No light-mode toggle.
- No SF Symbol / gradient substitute for the canonical logo.
- No `frame(maxWidth: 720)` cap on the main workspace.

## Evidence & Open Questions

Token values mirror the pre-existing `swift/Sources/Theme.swift` palette so the
PR-021 bootstrap is visually identical; later PRs may re-tune with contrast
audits.
