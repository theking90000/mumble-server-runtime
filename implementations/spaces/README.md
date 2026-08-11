# Spaces

Spaces is the provided named-room implementation. A controller assigns logical
participants to `SpaceKey` values, and Rust materializes each active Space as a
runtime shard and audio domain without exposing runtime identifiers.

- `protocol/` defines the typed Spaces payloads and pins their descriptor;
- `rust/` validates, stores, renders, and observes Spaces state;
- `sdk/java/` provides the typed Java 8 facade over Coordination.

The runnable host and Bukkit example still live temporarily under
`control-plane/`; later mechanical PRs move them into this vertical.
