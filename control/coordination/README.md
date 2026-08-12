# Coordination

Coordination is the runtime-independent remote synchronization layer.

- `protocol/` defines the language-neutral lifecycle and reliable-command wire
  contract;
- `rust/` implements ownership and reliable-request state machines;
- `sdk/java/` exposes the Java 8 client lifecycle and transport.

Payloads are bounded and tied to a negotiated profile reference, but
Coordination does not decode or own any concrete profile such as Spaces.
