# Remote control integration

This directory is the current, transitional home of both the generic remote
control framework and the provided Spaces implementation. It does not add
Controller network IO or business state to the runtime's core crates.

The target repository layout separates two sibling concerns:

```text
control/                    generic remote coordination and runtime adaptation
implementations/spaces/     provided named-Spaces model and runnable host
```

The word **controller** names a remote application using the protocol. Control
is the framework it talks to. Spaces is one complete implementation built on
that framework, not a concept owned by Coordination.

Current paths during the mechanical migration:

- `../control/coordination/protocol`: the generic Coordination Protobuf/gRPC contract;
- `../control/coordination/rust`: runtime-independent coordination state;
- `../control/coordination/sdk/java`: the Java 8 compatible Coordination SDK;
- `../control/runtime-adapter/rust`: the generic adapter to Mumble Server Runtime;
- `implementations/spaces/contract`: the versioned Spaces payloads;
- `implementations/spaces/rust`: the host-side Spaces model and rendering;
- `implementations/spaces/clients/java`: the typed Spaces SDK facade;
- `server-rust`: the runnable Spaces host, still at its transitional path;
- `reference/bukkit-spaces`: a buildable Spigot 1.8 plugin using the SDK, kept as
  a separate Gradle build.

The accepted target and dependency direction are recorded in
[`0008: Separate Control from provided implementations`](../docs/decisions/0008-control-and-implementations-layout.md).

The Java API is split between `be.theking90000.mumble.controller.core` and
`be.theking90000.mumble.controller.spaces`, and is published as:

```text
be.theking90000.mumble:controller-core
be.theking90000.mumble:controller-spaces
```

The generated public API reference is published as
[Mumble Controller JavaDoc](https://theking90000.github.io/mumble-server-runtime/controller/).

The shorter integration name is deliberately separate from the Mumble Server Runtime project
name. It also leaves sibling namespaces such as `be.theking90000.mumble.bukkit` available to
Minecraft adapters without nesting them below an implementation-specific runtime package.

The Rust process is a Spaces-specific composition crate. It depends on
Coordination, the Runtime Adapter, Spaces, and the gateway/shard runtime.
Coordination remains independent of Mumble and every concrete implementation.

The protocol model and its executable Java reducer are described in the
[`Controller integration`](../docs/book/controller/index.md) section of the book.

Run the integration checks from this directory:

```sh
./gradlew check
```

Run the Rust reducer and live integration checks from the repository root:

```sh
cargo test -p mumble-controller-server
ci/controller-interop.sh
```
