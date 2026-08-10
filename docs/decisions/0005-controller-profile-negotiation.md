# 0005: Negotiate typed Controller profiles

Status: accepted

## Context

A generic synchronization Core must route profile data without learning its
business vocabulary. Accepting arbitrary JSON or silently interpreting an
unknown schema would make validation and compatibility ambiguous.

## Decision

Each session negotiates a `profile_id`, `schema_version`, and pinned
`descriptor_digest` before it can mutate state. Unknown profiles, versions, or
descriptor digests are rejected before session creation.

Profile payloads are bounded Protobuf messages. They may be opaque to the Core,
but the selected compiled profile must decode and validate them before any
mutation. Internal runtime identifiers are never exposed through the profile
contract.

The contracts use independent packages:

- `mumble.controller.core.v1`
- `mumble.controller.spaces.v1`
- `mumble.controller.declarative.v1`

Descriptor fingerprints are pinned independently for each package.

## Consequences

- Core compatibility does not imply profile schema compatibility.
- Adding a profile requires a typed codec and explicit limits.
- There is no generic JSON escape hatch or best-effort fallback.
