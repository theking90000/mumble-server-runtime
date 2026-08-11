# Spaces

Spaces is the provided named-room implementation. A controller assigns logical
participants to `SpaceKey` values, and Rust materializes each active Space as a
runtime shard and audio domain without exposing runtime identifiers.

- `protocol/` defines the typed Spaces payloads and pins their descriptor;
- `rust/` validates and owns participant/Space state, renders it, and observes
  runtime events;
- `sdk/java/` provides the typed Java 8 facade over Coordination;
- `host/rust/` composes Coordination, the Runtime Adapter, Spaces, and the
  runtime into the runnable `mumble-controller-server` process;
- `bukkit/` is the buildable Java 8 consumer example, kept as a separate Gradle
  build so the SDK does not inherit a Minecraft dependency;
- `verification/spaces-interop/` independently exercises the real Java SDK,
  host, runtime, and simulated Mumble clients.
