# The runtime

A from-scratch Mumble server that speaks the protocol to unmodified clients.
Channels, visibility and audibility are computed from application state on every
render, rather than configured as a channel tree.

This is the lower of the two layers in the repository. A Rust application links
these crates directly and implements `ShardLogic`. An application in another
language reaches the same machinery through the declarative gRPC surface in
[`control-plane/`](../control-plane), built on top of it.

## Crates

| Path | Crate | Contents |
| --- | --- | --- |
| `crates/protocol` | `mumble-server-runtime-protocol` | Framing, protobuf messages, UDP envelopes. Pure. |
| `crates/crypto` | `mumble-server-runtime-crypto` | OCB2-AES128, IV recovery, replay window. Pure. |
| `crates/shard` | `mumble-server-runtime-shard` | Rendering and reconciliation of the shared view, per-connection composition, audio routing table. No IO at all. |
| `crates/gateway` | `mumble-server-runtime-gateway` | TLS, TCP, UDP, handshake, connection lifecycle, migration between shards. |
| `reference/arena` | `mumble-server-runtime-arena` | The worked demo. Every code fragment in the book comes from it. |
| `tools/stress` | `mumble-server-runtime-stress` | Load generator, with optional Opus voice. |
| `tools/bench-shard` | `bench-shard` | Cost of a single shard turn. |

Protocol truth is vendored under [`references/`](references), pinned to an exact
upstream commit. Any claim about the wire format traces back to it.

## Verification

`verification/` belongs to verification tasks. A change touching it and an
implementation at once is rejected by `ci/verifier-boundary.sh`.

The judge is `verification/testkit`, an independent simulated Mumble client that
applies the protocol against a strict model and panics on any violation. Around
it sit the binary captures of real sessions between an official client and
Murmur under `fixtures/`, fuzz targets for framing, control messages, UDP
envelopes and OCB2 under `fuzz/`, and three utilities under `protocol-tools/`:
`recording-proxy` records a session, `corpus-decode` turns a capture into a
readable transcript, and `mitm-proxy` relays between a client and Murmur.

## Running it

The demo listens on the address it is given:

```sh
cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738
```

Connect a Mumble client to `127.0.0.1`, not `localhost`, which resolves to IPv6
first and finds nothing listening.

Workspace checks run from the repository root, and the live tests among them
open real TLS and UDP loopback sockets:

```sh
RUSTFLAGS="-D warnings" cargo test --workspace --all-features
cargo run --release -p bench-shard
```

The concepts are stated in
[The model](https://theking90000.github.io/mumble-server-runtime/model/), and
writing an application against them in
[Building an application](https://theking90000.github.io/mumble-server-runtime/build/).
What an unmodified client gets, and how that claim is checked, is
[Mumble compatibility](https://theking90000.github.io/mumble-server-runtime/mumble/).
