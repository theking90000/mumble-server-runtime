# Mumble Controller

This directory contains the language-neutral controller contract, the Java SDK
that maintains a controller's desired participant state, and the composed Rust
server. It is an application integration and does not add Controller network IO
or business state to the runtime's core crates.

Current modules:

- `contract`: the canonical versioned Protobuf/gRPC contract;
- `sdk-java`: the Java 8 compatible `ControllerSession` SDK;
- `server-rust`: the composed gRPC, gateway and dynamic-Space server.

The Java API uses the package `be.theking90000.mumble.controller` and is published as:

```text
be.theking90000.mumble:controller
```

The generated public API reference is published as
[Mumble Controller JavaDoc](https://theking90000.github.io/mumble-server-runtime/controller/).

The shorter integration name is deliberately separate from the Mumble Server Runtime project
name. It also leaves sibling namespaces such as `be.theking90000.mumble.bukkit` available to
Minecraft adapters without nesting them below an implementation-specific runtime package.

The Rust server is an application crate. It depends on the gateway and shard
runtime, while the core crates remain independent of Controller vocabulary.

The protocol model and its executable Java reducer are described in the
[`Controller integration`](../../docs/book/controller/index.md) section of the book.

Run the integration checks from this directory:

```sh
./gradlew check
```

Run the Rust reducer and live integration checks from the repository root:

```sh
cargo test -p mumble-controller-server
ci/controller-interop.sh
```
