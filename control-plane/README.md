# Mumble Controller

A server process that turns declared state into live Mumble sessions. An
application connects over gRPC, states the voice arrangement it wants, and the
runtime materializes it. The application never speaks the Mumble protocol and
never links Rust.

This is the upper of the two layers in the repository, layered on
[`runtime/`](../runtime) rather than beside it. Rendering, reconciliation and
the Mumble wire protocol come from the runtime crates, which stay free of
Controller vocabulary.

## The protocol carries no voice vocabulary

One bidirectional stream carries session establishment and resume, lease
renewal, participant ownership with fencing tokens, reliable request replay,
desired-state reconciliation and resync. None of it names a voice concept.

Domain vocabulary travels as opaque profile payloads. The profile in use is
pinned during the handshake by identifier, schema version and descriptor digest,
and a mismatch is rejected before any session state exists.

- `core/contract`: the generic contract. One service, `Connect`, streaming
  `ClientFrame` against `ServerFrame`.
- `core/rust` (`mumble-controller-core`): the extensible core. Sessions,
  ownership, reliable replay, revision watermarks. Independent of the Mumble
  runtime, of Tokio and of any transport.
- `core/clients/java` (`controller-core`): the client-side counterpart.
  Transport, session lifecycle, reliable requests. Compiled against Java 8, so
  it runs on 8 and later.
- `host/rust` (`mumble-controller-host`): the Mumble adaptation. Credentials,
  live connection bindings, shard creation and migration, snapshot publication.

Any language with a gRPC stack can drive the contract. Java is the one with a
supplied core.

## Spaces is the reference profile

Spaces shows what a profile looks like, implemented on both sides and shipped
with the project. A named Space renders as one root channel and one audio
domain, leaving every participant in it mutually audible and inaudible to
everyone outside.

- `implementations/spaces/contract`: the profile's own messages, filling the
  holes the generic contract leaves open.
- `implementations/spaces/rust` (`mumble-controller-spaces`): the renderer.
- `implementations/spaces/clients/java` (`controller-spaces`): the client.
- `reference/bukkit-spaces`: a Spigot 1.8 plugin built on that client, keying a
  Space on each player's world. A demonstration, kept as a separate Gradle
  build.

Writing another profile means writing those first three parts. The core and the
host are unchanged by it.

## The composed server

`server-rust` (`mumble-controller-server`) joins the core, the host and one
compiled profile into the runnable process. `contract/` holds the superseded
combined contract, still generated as that crate's internal representation while
the migration away from it finishes; the served endpoint is the core one.

The Java side builds from this directory with `./gradlew check`. The Rust crates
and the live interoperability scenario run from the repository root:

```sh
cargo test -p mumble-controller-server
cargo test -p mumble-controller-core-conformance
ci/controller-interop.sh
```

`ci/controller-interop.sh` exercises the whole path: the Java client against the
Rust server against a strict Mumble client. `verification/core-conformance`
drives the public stream against a real server process, characterizing session
resume, capability rotation, takeover fencing, stale releases, request replay
and lease watermarks.

## Installing the Java clients

Both use the package `be.theking90000.mumble.controller`, under the intended
release coordinates `be.theking90000.mumble:controller-core` and
`be.theking90000.mumble:controller-spaces`. Neither is published to a registry
yet. Until then, install version `0.1.0-SNAPSHOT` locally from this directory:

```sh
./gradlew publishToMavenLocal
```

The integration name is deliberately separate from the Mumble Server Runtime
project name. It also leaves sibling namespaces such as
`be.theking90000.mumble.bukkit` available to Minecraft adapters without nesting
them below an implementation-specific runtime package.

Adding the dependency, running the server, declaring Spaces and participants and
connecting a player are covered in
[Controller integration](https://theking90000.github.io/mumble-server-runtime/controller/getting-started.html).
The reference pages in that section state the protocol model and the server's
runtime behavior, and the generated API is the
[Java client JavaDoc](https://theking90000.github.io/mumble-server-runtime/controller/).
