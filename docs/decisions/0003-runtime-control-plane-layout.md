# 0003: Separate runtime and control-plane trees

Status: accepted

## Context

The repository currently mixes the Mumble runtime, protocol verification,
composition tools, and the Controller integration at the root. Package names
identify their role, but filesystem ownership is not visible and structural
checks must enumerate unrelated root directories.

## Decision

Production Mumble runtime crates live under `runtime/crates/`. Independent
protocol verification lives under `runtime/verification/`. Runtime load tools
and reference compositions live under `runtime/tools/` and
`runtime/reference/`. Vendored protocol truth lives under
`runtime/references/`.

The Controller and its language clients live under `control-plane/`. The
runtime remains usable without the Controller, and the Controller composes the
runtime through explicit Rust host dependencies.

Rust package names and published API names do not change as part of this move.
The migration is mechanical and keeps production and verifier changes in
separate review diffs.

## Consequences

- Directory ownership is visible without interpreting package names.
- CI gates must resolve both layouts during the migration and only the new
  layout after it completes.
- Active documentation and build scripts must use the new paths.
- Historical text may retain old paths only when it is clearly marked as
  historical.
