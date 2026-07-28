# API documentation

The generated API documentation is published alongside this book, under
[`/api/`](../api/index.html). It is produced by `cargo doc` from the doc
comments in the workspace, and covers every public item of every crate.

Entry points, in the order a reader is likely to need them:

| crate | contents |
|---|---|
| [`voxloom_shard`](../api/voxloom_shard/index.html) | shared views, scopes, overlays, audio relations, deltas, the shard loop |
| [`voxloom_gateway`](../api/voxloom_gateway/index.html) | TLS control plane, handshake, connection routing, the voice plane |
| [`mumble_server_runtime_protocol`](../api/mumble_server_runtime_protocol/index.html) | TCP framing, control messages, UDP envelopes |
| [`mumble_server_runtime_crypto`](../api/mumble_server_runtime_crypto/index.html) | OCB2-AES128 and the per-connection crypto state |

A broken intra-doc link fails the build, so every link in the generated
documentation resolves.

To build the same documentation locally:

```console
$ cargo doc --workspace --no-deps --all-features --open
```
