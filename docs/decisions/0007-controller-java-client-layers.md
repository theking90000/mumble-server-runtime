# 0007: Separate the Java Core and Spaces client layers

Status: accepted

## Context

The first Java module split moved reusable primitives into `controller-core` and
the existing SDK into `controller-spaces`. The resulting artifacts have the
right dependency direction, but they still share one Java package and Spaces
still owns the generic session, participant ownership, reconnect, lease, and
reliable-command machinery.

That intermediate state cannot support a second typed profile without copying
the same correctness-critical client logic.

## Decision

The Java artifacts expose distinct packages and responsibilities:

| Artifact | Public package | Owns |
| --- | --- | --- |
| `controller-core` | `be.theking90000.mumble.controller.core` | sessions, reconnect, leases, ownership fencing, revisions, reliable commands, transport, generic participant state, profile references and bounded Protobuf payloads |
| `controller-spaces` | `be.theking90000.mumble.controller.spaces` | Space keys, typed participant specifications and status, observations, snapshots, the Spaces codec, and a thin facade over Core |

Generated Protobuf and gRPC classes remain internal implementation details.
Public Java methods do not expose generated messages or raw gRPC types.

The Core client treats profile payloads as opaque bytes associated with the
profile negotiated for the session. A concrete client module performs typed
encoding and decoding. Core owns request identifiers and completes reliable
profile-command futures; concrete profiles own the meaning of their command
and event payloads.

The generic participant handle owns registration identity, fencing tokens,
revision futures, connection credentials, release, and ownership lifecycle. A
Spaces participant handle wraps it and exposes only the typed Spaces desired
and observed state.

The composed server currently issues a credential accepted by its Mumble Host,
but the Core contract and Java API name it as an opaque connection credential.
Mumble-specific interpretation belongs to the Host and application
documentation, not to synchronization Core.

## Consequences

- `controller-spaces` depends on the public Core API, never on Core generated or
  package-private classes.
- `controller-core` never imports a concrete profile package.
- The two artifacts do not contribute classes to the same Java package.
- Spaces contains no transport, lease, reconnect, fencing, or reliable-request
  implementation.
- The legacy combined Controller Protobuf contract is removed after the Rust
  server consumes the Core and Spaces contracts directly.
- A higher-level public profile SPI is stabilized only after Declarative is a
  second concrete consumer. The low-level Core payload API is sufficient for
  the migration.
