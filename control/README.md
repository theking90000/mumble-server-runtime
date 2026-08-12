# Control

`control/` contains the reusable machinery for remotely controlling Mumble
Server Runtime. It deliberately contains no concrete voice model.

- `coordination/` owns the versioned protocol, runtime-independent Rust state,
  and Java SDK for sessions, leases, ownership fencing, revisions,
  deduplication, backpressure, and reliable commands;
- `runtime-adapter/` bridges implementation-owned snapshots and logical
  participants to Mumble Server Runtime.

Complete models belong under the repository-level `implementations/` tree.
Spaces is the first provided implementation. The dependency direction and
staged migration are recorded in
[`ADR 0008`](../docs/decisions/0008-control-and-implementations-layout.md).
