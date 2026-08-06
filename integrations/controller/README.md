# Mumble Controller

This directory contains the language-neutral controller contract and the Java SDK
that maintains a controller's desired participant state. It is an application
integration and does not add network IO or business state to the runtime's core
crates.

Current modules:

- `contract`: the canonical versioned Protobuf/gRPC contract;
- `sdk-java`: the Java 8 compatible `ControllerSession` SDK.

The Java API uses the package `be.theking90000.mumble.controller` and is published as:

```text
be.theking90000.mumble:controller
```

The generated public API reference is published as
[Mumble Controller JavaDoc](https://theking90000.github.io/mumble-server-runtime/controller/).

The shorter integration name is deliberately separate from the Mumble Server Runtime project
name. It also leaves sibling namespaces such as `be.theking90000.mumble.bukkit` available to
Minecraft adapters without nesting them below an implementation-specific runtime package.

`adapter-rust` is intentionally absent. A production adapter from the contract to
a concrete Rust flavor is a separate vertical slice.

The protocol lifecycle and its executable Java reducer are described in
[`docs/design/controller-integration.md`](../../docs/design/controller-integration.md).

Run the integration checks from this directory:

```sh
./gradlew check
```
