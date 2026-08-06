# Controller integration

This directory contains the language-neutral controller contract and the Java SDK
that maintains a controller's desired participant state. It is an application
integration and does not add network IO or business state to the runtime's core
crates.

Current modules:

- `contract`: the canonical versioned Protobuf/gRPC contract;
- `sdk-java`: the Java 8 compatible `ControllerSession` SDK.

`adapter-rust` is intentionally absent. A production adapter from the contract to
a concrete Rust flavor is a separate vertical slice.

The protocol lifecycle and its executable Java reducer are described in
[`docs/design/controller-integration.md`](../../docs/design/controller-integration.md).

Run the integration checks from this directory:

```sh
./gradlew check
```
