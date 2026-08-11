# Reference: the model

> Background material. A working integration does not require this page.

Mumble Controller is the language-neutral boundary for applications that own
participants but do not belong inside the voice runtime. A Minecraft server is
one possible controller; neither the contract nor the Java SDK depends on
Minecraft, Bukkit or Paper.

The integration uses three different kinds of identity:

- a **controller session** is one live declarative producer;
- a **participant** is a logical person owned by at most one session;
- a **Space** is a semantic placement key computed from participant state.

Controllers own participants, never runtime shards. Several sessions may place
their participants in the same Space. The Rust application decides how that
Space is materialized by the runtime, and clients never receive a `ShardId` or
`ConnectionId`.

## Desired state, not commands

Each participant carries one complete desired specification:

```text
ParticipantSpec {
    space_key
    display_name
    server_mute
    server_deaf
}
```

Changing `space_key` replaces the specification. There is no separate move
command, and no controller command creates or mutates a shard.

A session also owns a complete set of explicit Space observations. Its effective
read interest is:

```text
explicit observed Spaces
union Spaces containing one of its participants
```

`FetchSpace` is a one-shot read and never changes that set.

## Ownership and connection credentials

Rust issues an opaque ownership capability whenever a registration acquires a
participant. Every later write and release presents that exact capability. A
new acquisition replaces it, so a delayed release from the old owner cannot
detach the new one.

The Mumble join token delivered with a grant is a separate bearer credential.
It lets an unmodified Mumble client authenticate as that participant, but it
does not authorize controller writes. Applications treat it as a password and
never log it.

Transport liveness and the business lease are independent. Losing a gRPC stream
does not immediately discard ownership: the Java SDK reconnects with its full
desired snapshot while the server-side lease remains valid.

## Three watermarks

A successful gRPC write is not proof that a Mumble client has received a new
view. The protocol therefore keeps three milestones separate:

```text
accepted  -> the Controller application accepted the desired revision
applied   -> a valid runtime render consumed that revision
published -> the Mumble generation produced by that render
```

## Repository layout

The contract and the Java SDK live under `control-plane`:

- `contract` holds the canonical versioned Protobuf/gRPC definition. Its
  compiled descriptor digest is pinned in CI, so a wire-incompatible edit fails
  the build rather than reaching a release.
- `core/clients/java` holds common Java synchronization types;
- `implementations/spaces/clients/java` holds the Java 8 compatible Spaces client.
- `server-rust` holds the composed server described in
  [Reference: the Rust server](server.md).

See [Reference: lifecycles](java.md) for the session and participant state
machines.
