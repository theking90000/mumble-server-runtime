# Mumble Server Runtime

**A programmable, declarative Mumble server runtime.**

Mumble Server Runtime is to Mumble what [Minestom](https://minestom.net/) is to
Minecraft servers: a from-scratch, programmable server implementation that
speaks the Mumble protocol to unmodified clients, with the traditional server
model replaced by one driven entirely by application state.

No plugin, custom client or protocol extension is required. Instead of a
configured channel tree, channels, visibility and audibility are computed live
from application state.

> **Preliminary.** The runtime works and ships with a runnable demo. Nothing is
> published to a package registry yet, and the API is still work in progress.
> See [Status](#status) for what is real and what is not.

> **AI-assisted development.** Most of Mumble Server Runtime's implementation was delegated
> to AI coding agents. See the [AI development notice](#ai-assisted-development)
> at the end of this README for details about the process, safeguards and
> limitations.

## Two ways in

The repository holds two layers. Either one is a usable entry point.

|  | `runtime/` | `control-plane/` |
| --- | --- | --- |
| How an application connects | Links the crates and implements `ShardLogic` | Talks gRPC to a separate server process |
| What it writes | A render function, in Rust | Desired state, in any language with a gRPC stack |
| Voice structure | Arbitrary: channel trees, scopes, overlays, audio domains | Whatever the profile in use defines |

The control plane is built on the runtime rather than beside it. Rendering,
reconciliation and the Mumble wire protocol come from `runtime/`, and only the
declarative gRPC surface is added on top.

Its protocol names no voice concept. Sessions, ownership fencing, leases,
reliable replay and reconciliation are generic, and the domain vocabulary
travels as opaque profile payloads pinned during the handshake. A profile
supplies the meaning: Spaces, the reference profile shipped with the project,
renders each named Space as one root channel and one audio domain. Both a Rust
core and a Java core are supplied for building on the contract, along with the
Rust and Java implementations of Spaces and a Spigot plugin demonstrating them.

An application needing arbitrary voice topologies, written in Rust, targets the
runtime directly. An application needing a per-world or per-team voice split
from another language targets the control plane.

## The model

A conventional Mumble server has one channel tree shared by everyone, defined in
configuration and edited by hand, by admins or by scripts. Voice structure ends
up maintained alongside game state, and drifting from it.

Mumble Server Runtime inverts that. An application supplies a render function,
describing the intended voice session for the current application state. The
difference from what each connected client already holds is computed on every
render, and only that difference is sent.

Moving a player to a game, ending a round, promoting someone to spectator: the
application changes its state, and the voice session follows on the next render.
There is no second model to keep in sync, because there is no second model.

### Example

Two teams that cannot see each other, from one render:

```rust
impl ShardLogic for Match {
    /// What the voice session should look like *now*. Rendered once for the
    /// whole shard, whatever the number of connections.
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        let root = out.root("Match");
        let red = out.channel(root, RED_BASE, "Red Base", Narrow::Into(1));
        let blue = out.channel(root, BLUE_BASE, "Blue Base", Narrow::Into(2));

        for (connection, side) in &self.players {
            let base = if *side == Side::Red { red } else { blue };
            let who = Occupant::Connection(*connection);
            out.user(base, who, &self.name_of(*connection), Narrow::Same);
        }

        // Silence is the default. Each team hears itself, and nothing else.
        out.audio_domain(RED_VOICE, &self.team(Side::Red));
        out.audio_domain(BLUE_VOICE, &self.team(Side::Blue));
    }

    /// A red player observes the red scope. Blue Base is not filtered out of
    /// their view after the fact: it never enters it.
    fn observation(&mut self, connection: ConnectionId) -> ScopeSet {
        match self.players.get(&connection) {
            Some(side) => side.scope(),
            None => ScopeSet::NONE,
        }
    }

    /// A client asked for something. Nothing has moved yet: the application
    /// state changes here, and the next render carries it.
    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) { /* ... */ }
}
```

The runnable version of this, with spectators, a vanished admin and a migration
between two shards, is the
[arena demo](runtime/reference/arena).

## Handover between shards

Players connect once. The application moves them afterwards.

When a player leaves the hub for a match, the voice session is handed from one
shard to another. This is not a disconnect followed by a reconnect. The view the
client still holds is handed over by the source, a single transition onto it is
planned by the destination, and the client keeps one continuous connection, its
session, and every local setting attached to it. The world changes around it.

For a Minecraft network, this is roughly what BungeeCord and Velocity provide
for game servers, applied to voice instead.

One caveat, stated plainly because it is the kind of thing a README should not
blur: shards are units of rendering inside a single runtime process, not
separate machines. Moving a player between shards is implemented and covered by
tests. Spreading shards across hosts behind voice proxies is designed and not
built.

## Views differ per connection

Two clients on the same server can be sent entirely different channel trees at
the same instant, and each one is an ordinary Mumble session from the client's
point of view.

A red player sees red. A blue player sees blue. A spectator sees both. Staff see
a structure nobody else knows exists. None of this is permission filtering
applied to a shared tree after the fact: the trees are genuinely different.

## Visibility and audibility are separate

Being visible does not imply being audible, and audio is directional.

That separation expresses what a single shared tree cannot: two teams hidden
from each other while sharing a pre-match lobby, spectators who hear both teams
and are heard by neither, an admin invisible until they address a group,
proximity voice derived from distance rather than from channel membership.

## Running the demo

```sh
cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738
```

Point a Mumble client at `127.0.0.1`, port `64738`. Use the IP address, not
`localhost`, which resolves to IPv6 first and finds nothing listening.

The demo runs a lobby and an arena as two shards. Each of the three visibility
mechanisms is used for exactly one thing:

- teams as scopes, so a red player cannot see that blue exists,
- a vanished admin as a private overlay,
- spectators as one-way listeners, hearing both teams and heard by neither.

Double-click **> Enter the Arena** to migrate between the two shards without
losing the connection. Connect a third client with `overwatch` in the password
field to join as invisible staff.

For load testing, raise the admission ceiling and point the stress tool at it:

```sh
cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738 500
cargo run --release -p mumble-server-runtime-stress -- --clients 200 --duration 30s
```

## Status

**Works today**

- Unmodified Mumble clients connect and are sent divergent channel trees.
- Audio over UDP, with automatic fallback to the TCP tunnel when UDP is blocked.
- Migration between shards without dropping the connection.
- Text, context menu actions, self-mute and self-deafen routed through the
  application.
- A declarative gRPC control plane, extensible through profiles, exercised end
  to end against a real Mumble client in CI.

**How that is checked**

- Tests that open real TLS and UDP sockets, not mocks.
- An independent simulated client that applies the protocol and refuses any
  violation of its model.
- An interoperability scenario running the Java client against the Rust server
  against a strict Mumble client.
- Official Mumble clients, used throughout development.

**Not there yet**

- Nothing is published to a package registry. The Rust crates build from this
  workspace, and the Java clients install through `publishToMavenLocal` on
  version `0.1.0-SNAPSHOT`.
- API documentation is generated and published, but its coverage is uneven.
- The development chapter of the book is not written.
- The Spigot plugin under `control-plane/reference/bukkit-spaces/` demonstrates
  the shading recipe and the Java 8 target. It is a reference example, not a
  deployable product.
- No distributed topology: every shard lives in one runtime process.
- Opus only, forwarded without ever being decoded, so no server-side mixing.

## Repository layout

### `runtime/`

The Mumble server runtime, in Rust.

- `crates/shard`: rendering and reconciliation of the shared view,
  per-connection composition, audio routing table. No IO at all.
- `crates/gateway`: TLS, TCP, UDP, handshake, connections, migration between
  shards.
- `crates/protocol`, `crates/crypto`: the pure foundations. Framing, protobuf,
  UDP envelopes, OCB2.
- `reference/arena`: the worked demo above.
- `verification/`: the independent simulated client, fixtures, fuzzing and
  protocol tools.

Detailed in [`runtime/README.md`](runtime/README.md).

### `control-plane/`

The declarative Controller, layered on the runtime.

- `core/`: the generic contract and its two cores. Sessions, ownership fencing,
  reliable replay and reconciliation, with no voice vocabulary. Implemented in
  Rust for the server side and in Java for the client side.
- `host/`: Mumble adaptation. Credentials, connection bindings, shard
  operations.
- `implementations/spaces/`: the reference profile, in Rust and in Java. One
  root channel and one audio domain per named Space.
- `server-rust/`: the composed server process.
- `reference/bukkit-spaces/`: a Spigot plugin demonstrating the Spaces client.
- `verification/`: a black-box verifier for the public gRPC stream.

Detailed in [`control-plane/README.md`](control-plane/README.md).

Working rules live in [`AGENT.md`](AGENT.md). The earlier exploratory pipeline
remains readable at the `legacy-p7-final` tag.

## Documentation

- **[The book](https://theking90000.github.io/mumble-server-runtime/)**: what the
  runtime is, then how to build on it. Start at
  [The model](https://theking90000.github.io/mumble-server-runtime/model/) for
  the concepts, or
  [Building an application](https://theking90000.github.io/mumble-server-runtime/build/)
  to write one.
  [Mumble compatibility](https://theking90000.github.io/mumble-server-runtime/mumble/)
  states what an unmodified client gets, and how that claim is checked.
- **[Controller integration](https://theking90000.github.io/mumble-server-runtime/controller/getting-started.html)**:
  the control plane from an application author's point of view. Adding the Java
  dependency, running the server, declaring Spaces and participants, connecting
  a player.
- **[API documentation](https://theking90000.github.io/mumble-server-runtime/api/)**:
  generated from the doc comments, every public item of every crate.
- **[Java client JavaDoc](https://theking90000.github.io/mumble-server-runtime/controller/)**:
  the data model, lifecycle and complete public API of the Spaces client.
- **[`runtime/reference/arena/`](runtime/reference/arena)**:
  the worked example, about 1200 lines. Every fragment in the book comes from it.

The published pages are built from `main` by the `Pages` workflow. To build them
locally:

```sh
mdbook serve --open                                    # the book
cargo doc --workspace --no-deps --all-features --open  # the API
```

## Development

```sh
ci/gates.sh                 # structural prohibitions
ci/dep-direction.sh         # dependency direction between crates
ci/verifier-boundary.sh     # implementer and verifier stay separate
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
RUSTFLAGS="-D warnings" cargo test --workspace --all-features
cargo run --release -p bench-shard      # cost of one shard turn
(cd control-plane && ./gradlew check)   # gRPC contract and Java clients
ci/controller-interop.sh                # Java client -> Rust server -> Mumble
```

The live tests open loopback sockets. The toolchain is pinned in
`rust-toolchain.toml` (Rust 1.93, edition 2024).

## License

Mumble Server Runtime is licensed under the
[Apache License, Version 2.0](LICENSE).

The protocol references and test vectors under `runtime/references/vendored/` retain
their upstream licenses and provenance.

## Acknowledgements

Mumble Server Runtime is an independent implementation of the
[Mumble](https://www.mumble.info/) protocol. It is not affiliated with or
endorsed by the Mumble project.

Protocol compatibility work relies on the Mumble project's source code and
message schemas, developed by the Mumble Developers and distributed under a
BSD-style license. The excerpts and test vectors kept under
`runtime/references/vendored/` retain the upstream copyright and license; see
[`runtime/references/vendored/LICENSE`](runtime/references/vendored/LICENSE) and
[`runtime/references/vendored/PROVENANCE.md`](runtime/references/vendored/PROVENANCE.md).

## AI-assisted development

Mumble Server Runtime was designed by a human and implemented almost entirely by AI coding
agents. The author invested substantial time in product definition, software
architecture, specifications, failure analysis, review criteria and verification,
while delegating most of the implementation work to agents.

This may still be called "vibe coding", depending on how one defines the term.
AI coding agents are becoming increasingly mature and capable, and Mumble Server Runtime was
also an opportunity to explore what AI-assisted software development can look
like on a substantial systems project.

That exploration was not unconstrained. The agents worked within detailed written
specifications, explicit architectural boundaries, fail-closed safeguards,
vendored protocol references, structural CI gates and an extensive test suite,
including tests against real protocol and networking behavior. The author remained
responsible for the design, specifications, architectural decisions and acceptance
criteria, while delegating most of the implementation work to agents.

These boundaries and safeguards increase confidence in the implementation, but
they do not guarantee its correctness. Mumble Server Runtime may still contain bugs, security
issues, protocol incompatibilities or other unexpected behavior. The project is
provided "as is", without warranties or guarantees of any kind.
