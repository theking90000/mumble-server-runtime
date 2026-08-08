# 0004: Separate Controller Core, Host, and profiles

Status: accepted

## Context

The first Controller implementation combines reusable synchronization safety,
Mumble runtime placement, and the named-Space business model. A second concrete
consumer is required before the reusable boundary can be made public.

## Decision

The Controller is split into four responsibility layers:

| Layer | Owns | Must not own |
| --- | --- | --- |
| Core | sessions, leases, fencing, revisions, request deduplication, bounded queues, reliable commands | Mumble connections, channels, Spaces, rendering |
| Host | Mumble authentication, logical participant to connection mapping, immutable snapshots, publication correlation, `watch` and `wake()` | profile policy, lease ownership |
| Profile | typed business state, payload validation, rendering, observations, profile commands | tokens, leases, acknowledgements, direct publication |
| Server | compiled profile selection, transport, configuration, process lifecycle | business state |

```text
controller-server
    -> controller-spaces or controller-declarative
    -> controller-host
    -> controller-core

controller-host
    -> runtime/gateway
    -> runtime/shard

controller-core
    -/-> runtime
    -/-> concrete profiles
```

Profiles are selected from implementations compiled into the server. Dynamic
plugin loading is not supported. Spaces preserves the current behavior.
Declarative will be the second concrete implementation and will materialize a
validated voice scene without calling Java during rendering.

## Consequences

- The initial repository move does not claim that these extracted crates exist.
- A reusable trait is stabilized only after Spaces and Declarative both use it.
- Java is the first official client, not part of the protocol definition.
