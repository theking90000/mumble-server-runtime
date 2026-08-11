# Mumble Server Runtime

**A programmable, declarative Mumble server runtime.**

Mumble Server Runtime aims to be for Mumble what
[Minestom](https://minestom.net/) is for Minecraft servers: a from-scratch,
programmable server implementation that speaks the Mumble protocol to unmodified
clients while replacing the traditional server model with one driven entirely
by application state.

No plugin, custom client or protocol extension is required. Instead of a channel
tree you configure, channels, visibility and audibility are computed live from
your application's state.

> **Preliminary.** The runtime and the provided Spaces remote-control host work.
> The published APIs are still work-in-progress. A buildable Bukkit example is
> included, but it is not a production-ready plugin. See [Status](#status) for
> what is real and what is not.

> **AI-assisted development.** Most of Mumble Server Runtime's implementation was delegated
> to AI coding agents. See the [AI development notice](#ai-assisted-development)
> at the end of this README for details about the process, safeguards and
> limitations.

## The idea

A conventional Mumble server has one channel tree shared by everyone, defined in
configuration and edited by hand, by admins or by scripts. Voice structure ends
up being maintained alongside your game state, and drifting from it.

Mumble Server Runtime inverts that. You write a render function. It describes what the voice
session should look like _right now_, given your state. Mumble Server Runtime works out the
difference from what each connected client currently holds, and sends only that.

Move a player to a game, end a round, promote someone to spectator: change your
state, and the voice session follows on the next render. There is no second model
to keep in sync, because there is no second model.

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

    /// A client asked for something. Nothing has moved: change your state, and
    /// the next render carries it.
    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply) { /* ... */ }
}
```

The runnable version of this, with spectators, a vanished admin and a migration
between two shards, is the
[arena demo](runtime/reference/arena).

## The BungeeCord of voice

Players connect once. After that, your application moves them.

When a player leaves the hub for a match, their voice session is handed from one
shard to another. This is not a disconnect followed by a reconnect. The source
hands over the view the client still holds, the destination plans a single
transition onto it, and the client keeps one continuous connection, its session,
and every local setting it has attached to it. The world changes around it.

If you run a Minecraft network, this is roughly what BungeeCord and Velocity give
you for game servers, applied to voice instead.

One caveat, stated plainly because it is the kind of thing a README should not
blur: shards are units of rendering inside a single Mumble Server Runtime runtime, not separate
machines. Moving a player between shards is implemented and covered by tests.
Spreading shards across hosts behind voice proxies is designed and not built.

## Views differ per connection

Two clients on the same server can be sent entirely different channel trees, at
the same instant, and each one is an ordinary Mumble session as far as the client
is concerned.

A red player sees red. A blue player sees blue. A spectator sees both. Staff see
a structure nobody else knows exists. None of this is permissions filtering a
shared tree after the fact: the trees are genuinely different.

## Visibility and audibility are separate

Being visible does not imply being audible, and audio is directional.

That is what lets you express things a single shared tree cannot: two teams
hidden from each other while sharing a pre-match lobby, spectators who hear both
teams and are heard by neither, an admin who is invisible until they address a
group, proximity voice derived from distance rather than from channel membership.

## Try it

```sh
cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738
```

Then point a Mumble client at `127.0.0.1`, port `64738`. Use the IP address, not
`localhost`, which resolves to IPv6 first and finds nothing listening.

The demo is a lobby and an arena running as two shards, and each of the runtime's
three visibility mechanisms is used for exactly one thing:

- teams as scopes, so a red player cannot see that blue exists,
- a vanished admin as a private overlay,
- spectators as one-way listeners, hearing both teams and heard by neither.

Double-click **> Enter the Arena** to migrate between the two shards without
losing the connection. Connect a third client with `overwatch` in the password
field to join as invisible staff.

To load-test it, raise the admission ceiling and point the stress tool at it:

```sh
cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738 500
cargo run --release -p mumble-server-runtime-stress -- --clients 200 --duration 30s
```

## Status

**Works today**

- Unmodified Mumble clients connect and are sent divergent channel trees.
- Audio over UDP, falling back to the TCP tunnel on its own when UDP is blocked.
- Migration between shards without dropping the connection.
- Text, context menu actions, self-mute and self-deafen routed through your
  application.

**How that is checked**

- Tests that open real TLS and UDP sockets, not mocks.
- An independent simulated client that applies the protocol and refuses any
  violation of its model.
- Official Mumble clients, used throughout development.

**Not there yet**

- API documentation is generated and published, but its coverage is uneven.
- The book documents the model, how to build on it, and what a Mumble client
  gets. The development chapter is not written yet.
- The Bukkit integration is a worked example rather than a supported production
  plugin. Applications can embed the Rust runtime directly or use the provided
  Spaces host from Java.
- No distributed topology: every shard lives in one runtime.
- Opus only, forwarded without ever being decoded, so no server-side mixing.

## How it fits together

Two crates carry the runtime:

- `mumble-server-runtime-shard` renders and reconciles the shared view, composes the private
  views and publishes an audio routing table. It performs no IO at all.
- `mumble-server-runtime-gateway` handles TLS, TCP, UDP, the handshake, connections and
  migration between shards.

`mumble-server-runtime-protocol` and `mumble-server-runtime-crypto` are the pure foundations: framing,
protobuf, UDP envelopes, OCB2. `runtime/reference/arena` is the demo above.
`mumble-server-runtime-testkit` is the independent judge, a simulated Mumble client that
applies the protocol and refuses any violation of its strict model.

Remote integration has two layers. **Control** provides reusable coordination
for many remote controllers targeting one host, plus the adapter to Mumble
Server Runtime. **Spaces** is the provided implementation: its Java SDK assigns
participants to named Spaces and its Rust host materializes them as runtime
shards without exposing shard identifiers. The generic pieces live under
`control/`, and the Spaces protocol, Rust model, and Java SDK live under
`implementations/spaces/`, including the runnable host and Bukkit example. Only
the temporary Gradle composition build remains under `control-plane/`.

This creates three entry points:

1. use `runtime/` directly from Rust for full control;
2. use Control Coordination to build a different remote model;
3. use the provided Spaces SDK and host without designing a voice model.

The current architecture reference is the [`docs/book/`](docs/book) source. The
working rules live in [`AGENT.md`](AGENT.md). The earlier exploratory pipeline
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
- **[API documentation](https://theking90000.github.io/mumble-server-runtime/api/)**:
  generated from the doc comments, every public item of every crate.
- **[Mumble Controller JavaDoc](https://theking90000.github.io/mumble-server-runtime/controller/)**:
  the Java 8 SDK data model, lifecycle and complete public API.
- **[`runtime/reference/arena/`](runtime/reference/arena)**:
  the worked example, about 1200 lines. Every fragment in the book comes from it.

The first two are published from `main` by the `Pages` workflow. To build them
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
cargo run --release -p bench-shard   # cost of one shard turn
(cd control-plane && ./gradlew check) # controller contract + Java 8 SDK
ci/controller-interop.sh       # real Java SDK -> Rust server -> Mumble path
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
