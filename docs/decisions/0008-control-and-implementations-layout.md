# 0008: Separate Control from provided implementations

Status: accepted

## Context

The repository currently groups the generic remote-control machinery, the
provided Spaces model, its runnable Rust host, and the Bukkit example under
`control-plane/`. The Java API and the Protobuf contracts already separate Core
from Spaces, but the filesystem still presents Spaces as a child of the generic
control mechanism and presents the Spaces-only process as a generic server.

That layout hides three different integration choices:

1. embed Mumble Server Runtime directly from Rust;
2. reuse remote coordination while defining a different business model;
3. use the provided Spaces implementation without designing a model.

## Decision

Generic remote-control code lives under `control/`. Complete business models
provided by this repository live under the sibling `implementations/` tree.

```text
runtime/

control/
├── coordination/
│   ├── protocol/
│   ├── rust/
│   ├── sdk/java/
│   └── verification/
└── runtime-adapter/rust/

implementations/
└── spaces/
    ├── protocol/
    ├── rust/
    ├── sdk/java/
    ├── host/rust/
    ├── bukkit/
    └── verification/
```

`coordination` owns reusable many-controllers-to-one-host correctness: sessions,
resumption, leases, ownership fencing, revisions, request deduplication,
backpressure, reliable commands, and exact implementation negotiation. It does
not know Mumble, runtime shards, or Spaces.

`runtime-adapter` owns the bridge to Mumble Server Runtime: connection
credentials, logical-participant-to-connection association, immutable snapshots,
runtime operations, and application/publication correlation. It does not own
coordination policy or a concrete business model.

An implementation is a complete vertical model. Spaces owns its typed protocol,
host-side Rust behavior, Java SDK facade, runnable host composition, and Bukkit
example. The runnable process currently called `mumble-controller-server` is a
Spaces host and moves to `implementations/spaces/host/rust`.

The word `profile` remains the protocol concept selected by `profile_id`, schema
version, and descriptor digest. It does not name a repository ownership layer.
Unknown profile references are rejected before a session can mutate state.

The Core and Spaces Protobuf package names, Maven coordinates, and Rust package
names do not change as part of the filesystem migration. Renaming a public or
wire identity requires a separate decision.

## Dependency direction

```text
spaces-host
    -> spaces-rust
    -> control-coordination
    -> control-runtime-adapter

spaces-rust
    -> control-coordination
    -> control-runtime-adapter

control-runtime-adapter
    -> runtime

spaces-sdk-java
    -> coordination-sdk-java

control-coordination
    -/-> runtime
    -/-> concrete implementations

control-runtime-adapter
    -/-> concrete implementations
```

The Spaces host may depend on every layer because it is the composition root.
The Spaces implementation must not issue session or ownership capabilities,
renew leases, acknowledge Core revisions, or implement reliable-request replay.

## Migration

The migration is intentionally staged:

1. document the ownership model while paths still use `control-plane/`;
2. teach structural gates both layouts without moving production code;
3. move Coordination and Runtime Adapter mechanically;
4. move verifier-owned code in a separate review diff;
5. move the Spaces protocol, Rust library, Java SDK, host, and Bukkit example;
6. remove support for the old paths from CI;
7. extract the remaining Spaces business state from the host actor.

Mechanical moves do not change behavior or descriptor digests. Implementation
and independent verifier changes remain separate under R2.

## Consequences

- The repository root exposes the three integration choices directly.
- Spaces is described as a provided implementation, not as the ceiling of the
  remote-control framework.
- A second implementation can be added without nesting it below generic Control.
- The current path remains temporarily valid while the stacked migration is in
  progress, but active documentation names both the current and target layout.
- A public Rust implementation SPI is not stabilized until two concrete
  implementations need the same boundary.
