# 0006: Separate Controller flow classes

Status: accepted

## Context

Lifecycle correctness, desired-state convergence, reliable commands, and
high-rate positional input have different delivery and saturation semantics.
Sharing one queue or lease-renewal path would allow optional realtime traffic
to compromise ownership.

## Decision

The Controller defines four flow classes:

| Flow | Delivery model | Backpressure and recovery |
| --- | --- | --- |
| Lifecycle | ordered and reliable | reserved capacity; failure closes or resumes the session explicitly |
| Desired state | revisioned and coalesced | newest complete revision wins; reconnect sends a full snapshot |
| Reliable commands | request ID plus terminal result | bounded queue, deduplication, explicit success or rejection |
| Realtime | epoch and sequence, latest-wins | independent bounded capacity, no replay, full snapshot after reconnect |

Realtime traffic uses a stream separate from lifecycle and lease renewal. Its
saturation cannot delay ownership fencing or session expiry. The first planned
consumer is a bounded `PositionBatch`; it is not introduced by the repository
move.

## Consequences

- Every message belongs to exactly one delivery class.
- Realtime loss is observable but does not imply lifecycle failure.
- Reliable commands never inherit latest-wins semantics.
