# Reference: the Spaces Rust host

> Internals and complete option list. [Getting started](getting-started.md)
> covers what is needed to run the process.

`mumble-spaces-server` is the runnable host for the provided
Spaces implementation. It composes the generic Coordination protocol, the
Spaces payload protocol, the Runtime Adapter, and the gateway/shard runtime.
Its source lives at `implementations/spaces/host/rust`; the existing package and
binary use the implementation-specific `mumble-spaces-server` name.

One Tokio actor currently serializes session leases,
participant ownership, join credentials, observations, Space lifetimes and
Mumble connection bindings. There is no distributed lock or application-level
compare-and-swap operation.

The target architecture moves generic session, lease, fencing, revision and
reliable-request decisions behind Coordination while Spaces keeps observations,
Space lifetimes and snapshots. The process remains the composition root for the
concrete Spaces implementation.

Each materialized Space owns one dynamic shard. Its `ShardLogic` reads an
immutable, revisioned snapshot published by the actor and never waits for gRPC.
The composition-level reconciliation observer associates the snapshot revision
consumed by `render()` with the resulting runtime generation. This is what lets
the protocol report accepted, applied and published as separate watermarks.

## Starting the process

A persistent Mumble certificate is supplied as PEM files:

```sh
cargo run -p mumble-spaces-server -- \
  --mumble-cert server-cert.pem \
  --mumble-key server-key.pem
```

For explicit local development only:

```sh
cargo run -p mumble-spaces-server -- --dev-self-signed
```

The Controller listener defaults to `127.0.0.1:4000`. Binding it to a
non-loopback address requires
`--allow-unauthenticated-controller-network`, because v1 is plaintext and does
not authenticate Java sessions. The Mumble listener defaults to
`0.0.0.0:64738` on both TLS/TCP and UDP.

| Option | Default |
|---|---:|
| `--lease-seconds` | 30 |
| `--empty-space-grace-seconds` | 30 |
| `--max-sessions` | 64 |
| `--max-participants` | 10,000 |
| `--max-participants-per-session` | 5,000 |
| `--max-spaces` | 1,024 |
| `--max-observations-per-session` | 1,024 |
| `--queue-capacity` | 1,024 |
| `--grpc-max-frame-bytes` | 4 MiB |
| `--max-mumble-connections` | 100 |

## Lifecycle

A lost gRPC stream does not release participants. Their ownership and Mumble
connections remain until the 30-second business lease expires. `CloseSession`,
an exact-token participant release, or lease expiry closes the corresponding
Mumble connection immediately.

The first participant placed in a Space creates its shard. Disconnected
participants remain in the read-only `SpaceSnapshot` but are not rendered as
Mumble users. When the last participant leaves, the shard remains for a
30-second grace period. A later materialization of the same key receives a new
opaque incarnation identifier.

The real interoperability check is:

```sh
ci/controller-interop.sh
```

It starts the real Rust server, drives it through a Java 8 `ControllerSession`,
and connects strict simulated Mumble clients over TLS and UDP. Join credentials
travel through a private temporary file and are never printed.
