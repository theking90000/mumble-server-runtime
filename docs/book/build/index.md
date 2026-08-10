# Building an application

How to write one. Types, signatures and code appear throughout, and the
generated [API documentation](../reference/api.md) carries the detail this
chapter leaves out.

[Running the arena](arena.md) needs nothing but a Mumble client and comes first
for that reason. [Anatomy of an application](anatomy.md) is the shortest path to
understanding what the demonstration is made of.

The pages after those two follow the order the work itself takes: rendering a
view, deciding where a distinction belongs, declaring who hears whom, answering
what clients do, moving connections between shards, and measuring the result.

Terms used here without definition are defined in
[The model](../model/index.md).

## The example to read

Every code fragment in this chapter is drawn from the demonstration
application, which lives under `runtime/reference/arena/` and builds
with the workspace.

| file | what to take from it |
| --- | --- |
| `src/main.rs` | composition: gateway, two shards, an external update task |
| `src/lobby.rs` | the simplest shard that can exist, and a migration request |
| `src/arena.rs` | scopes, overlays and the audio relation, one purpose each |
| `src/router.rs` | where an arrival goes, and what a credential buys |
| `src/directory.rs` | state shared between shards, and shard discovery |
| `tests/arena.rs` | driving a shard turn by turn without sockets |

About 1200 lines of application code and 700 of tests, and the only crate in the
repository naming both the runtime and a concrete business. Copying its shape is
a reasonable way to start.
