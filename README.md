# Mumble Server Runtime

**A Mumble-compatible voice server your application drives.**

Mumble Server Runtime speaks the Mumble protocol to unmodified clients. No plugin, no custom
client, no protocol extension. What it does not have is a channel tree you
configure: the channels, who sees whom and who hears whom are all computed from
your application's state, live.

> **Preliminary.** The runtime works and ships with a runnable demo, but this
> README is ahead of its documentation. The published API is still work-in-progress,
> there is no integration guide, and no ready-made plugin for any game. See
> [Status](#status) for what is real and what is not.

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

- API documentation still in progress, though `cargo doc` already builds it.
- No integration guide.
- No plugin or bridge for any game, Minecraft included. Today you write Rust
  against the runtime directly.
- No distributed topology: every shard lives in one runtime.
- Opus only, forwarded without ever being decoded, so no server-side mixing.

## How it fits together

Two crates carry the runtime:

- `mumble-server-runtime-shard` renders and reconciles the shared view, composes the private
  views and publishes an audio routing table. It performs no IO at all.
- `mumble-server-runtime-gateway` handles TLS, TCP, UDP, the handshake, connections and
  migration between shards.

`mumble-server-runtime-protocol` and `mumble-server-runtime-crypto` are the pure foundations: framing,
protobuf, UDP envelopes, OCB2. `tools/mumble-server-runtime-arena` is the demo above.
`mumble-server-runtime-testkit` is the independent judge, a simulated Mumble client that
applies the protocol and refuses any violation of its strict model.

The architecture reference is
[`docs/design/guide-implementation.md`](docs/design/guide-implementation.md).
Detailed state lives in [`docs/STATUS.md`](docs/STATUS.md) and the working rules
in [`AGENT.md`](AGENT.md). The earlier exploratory pipeline remains readable at
the `legacy-p7-final` tag.

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
```

The live tests open loopback sockets. The toolchain is pinned in
`rust-toolchain.toml` (Rust 1.93, edition 2024).

## License

Mumble Server Runtime is licensed under the
[Apache License, Version 2.0](LICENSE).

The protocol references and test vectors under `references/vendored/` retain
their upstream licenses and provenance.

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
