# Spaces

Spaces is the provided named-room implementation. A controller assigns logical
participants to `SpaceKey` values, and Rust materializes each active Space as a
runtime shard and audio domain without exposing runtime identifiers.

- `protocol/` defines the typed Spaces payloads and pins their descriptor;
- `rust/` validates, stores, renders, and observes Spaces state;
- `sdk/java/` provides the typed Java 8 facade over Coordination;
- `host/rust/` composes Coordination, the Runtime Adapter, Spaces, and the
  runtime into the runnable `mumble-controller-server` process.

The Bukkit example still lives temporarily under `control-plane/`; a later
mechanical PR moves it into this vertical.
