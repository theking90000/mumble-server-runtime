# API documentation

The generated Rust API documentation is published alongside this book, under
[`/api/`](../api/index.html). It is produced by `cargo doc` from the doc comments
in the workspace, and covers every public item of every crate.

Entry points, in the order a reader is likely to need them:

| crate | contents |
|---|---|
| [`mumble_server_runtime_shard`](../api/mumble_server_runtime_shard/index.html) | shared views, scopes, overlays, audio relations, deltas, the shard loop |
| [`mumble_server_runtime_gateway`](../api/mumble_server_runtime_gateway/index.html) | TLS control plane, handshake, connection routing, the voice plane |
| [`mumble_server_runtime_protocol`](../api/mumble_server_runtime_protocol/index.html) | TCP framing, control messages, UDP envelopes |
| [`mumble_server_runtime_crypto`](../api/mumble_server_runtime_crypto/index.html) | OCB2-AES128 and the per-connection crypto state |

A broken intra-doc link fails the build, so every link in the generated
documentation resolves.

The Java 8 controller SDK has a separate generated
[`JavaDoc entry point`](../controller/index.html). It documents the declarative
participant model, ownership lifecycle, read-only spaces and asynchronous
completion semantics exposed by `be.theking90000.mumble.controller`.

To build the same documentation locally:

```console
$ cargo doc --workspace --no-deps --all-features --open
$ cd control-plane && ./gradlew javadoc
```
