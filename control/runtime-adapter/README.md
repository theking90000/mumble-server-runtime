# Runtime Adapter

The Runtime Adapter is the generic bridge from a remotely coordinated model to
Mumble Server Runtime. It owns runtime-facing connection and immutable snapshot
mechanics, not sessions, leases, fencing, or model-specific business state.

Concrete implementations consume this adapter. The adapter must never depend
on Spaces or another implementation.
