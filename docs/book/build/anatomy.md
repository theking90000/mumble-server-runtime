# Anatomy of an application

An application built on the runtime consists of four parts:

- **application state**, owned entirely by the application and never inspected
  by the runtime,
- **one or more shards**, each an implementation of `ShardLogic` describing what
  a voice session should look like given that state,
- **a router**, deciding where an arriving connection is attached,
- **a composition binary**, wiring the above onto a gateway.

Only the last three name runtime types. The first is whatever the application
already is.

## Two crates

`mumble-server-runtime-shard` renders, plans and composes. It never learns what
a socket is. `mumble-server-runtime-gateway` is everything the shard crate
deliberately does not know: TLS, TCP, UDP, the handshake, the connection
registry and migration.

```text
  TCP+TLS  -> connection task -> router.route() -> Attach(shard)
                    |                                   |
                    | control frames                    | commands, wake
                    v                                   v
              output queue  <-------------------  the shard's task

  UDP      -> voice plane -> bindings -> the shard's routing table
                             (no shard task is involved, ever)
```

The two planes meet at exactly two places: a routing table published by the
shard and read without a shard's help, and a per-connection cursor the shard
advances and the voice plane compares against. A shard task never awaits IO, and
the voice plane never waits for a shard.

## `ShardLogic`

Three methods, and the split between them is the interface.

```rust
pub trait ShardLogic: Send + 'static {
    fn render(&mut self, out: &mut ShardBuilder<'_>);
    fn observation(&mut self, connection: ConnectionId) -> ScopeSet;
    fn observe(&mut self, event: &VoiceEvent, out: &mut Reply);
}
```

**`render`** builds the shared view, the private overlays and the audio
relation. It takes no viewer parameter, which is the point: what is shared
cannot depend on who is looking. It is synchronous, and must neither await nor
block.

**`observation`** answers what one connection sees of the shared view. Called
once per connection per turn, so the returned value stays a small `Copy` value.
Returning something larger is how the cost goes quadratic again.

**`observe`** receives a voice fact and decides what to do with it. The `Reply`
is the only channel through which the logic speaks. Everything it wants to
*change* belongs to the next render instead.

The trait is `Send + 'static` and deliberately not `Sync`. The runtime never
shares the logic, so it imposes no synchronization.

## One turn

```text
   application state changes
        |  wake()
        v
  +------------------------------------------------------+
  | the shard's task (woken, one per shard)              |
  |                                                      |
  |  1. logic.render(&mut builder)                       |
  |       -> the SHARED view + the PRIVATE overlays      |
  |          + the AUDIO relation                        |
  |  2. ops = plan(current view -> new view)             |
  |  3. version += 1, ops -> journal                     |
  |  4. publish the audio routing table                  |
  |  5. for each connection:                             |
  |       its observation changed? -> replan for it      |
  |       otherwise -> filter, splice, collapse, encode, |
  |                    push, advance its committed state |
  +------------------------------------------------------+
```

Two orderings in that loop are load-bearing rather than incidental. The routing
table is published before the views are pushed, so a revoked route disappears
immediately. And the committed state of a connection is a quadruplet, cursor,
observation, overlay and offered actions, whose four parts advance together once
the queue has accepted everything.

A render that fails validation never replaces the current view. It is logged,
the previous view is kept, and nothing is closed. See
[Refusals](../model/refusals.md).

Consecutive turns are separated by at least 50 ms. Several wake-ups arriving
inside that window are coalesced into one turn.

## Reaching a render from outside

`wake` carries no data. It states only that the desired state may have changed.
The transport for application vocabulary belongs to the application, which keeps
its receiving end inside the logic and hands the sending end to whatever
produces events.

```rust
struct GameLogic {
    inbox: mpsc::Receiver<GameEvent>,
    state: GameState,
}

impl ShardLogic for GameLogic {
    fn render(&mut self, out: &mut ShardBuilder<'_>) {
        while let Ok(event) = self.inbox.try_recv() {
            self.state.apply(event);
        }
        self.state.render(out);
    }
    // observation, observe
}
```

The producer publishes first, then wakes:

```rust
async fn publish(
    notification: Notification,
    sender: &mpsc::Sender<GameEvent>,
    handle: &ShardHandle,
) -> Result<(), mpsc::error::SendError<GameEvent>> {
    let event = calculate(notification).await;
    sender.send(event).await?;
    handle.wake();
    Ok(())
}
```

Publishing before waking is what guarantees the next render observes the change.
Any `.await` belongs on the producer side, outside the shard task.

An ordered stream of events suits an `mpsc::Receiver`. State where the last
value wins suits a `watch::Receiver` holding an immutable snapshot. Either way
the channel remains the source of truth, since wake-ups may be coalesced.

## Routing

A router answers where an arrival begins, not where a player belongs. The second
question is the logic's, and its answer is a migration.

```rust
pub trait ConnectionRouter: Send + Sync + 'static {
    fn route(
        &self,
        connection: ConnectionId,
        identity: &ConnectionIdentity,
    ) -> impl Future<Output = RouteDecision> + Send;
}
```

`ConnectionIdentity` carries a name, an optional certificate hash and an opaque
credential. Everything in it is a claim except the certificate hash, and what
any of it buys is decided by the application. `RouteDecision` is either
`Attach(ShardId)` or `Reject(String)`, where the string reaches the client.

Routing runs on the connection's own task, so it may await a database or an
authentication service without holding up anything else. The cost is borne by
the arriving client alone.

Routing is also the only moment where a `ConnectionId` and the claim behind it
meet. Later on, a `VoiceEvent` carries the identifier and nothing else, since
the runtime holds no opinion on what a user is. An application whose logic needs
a name records the pair here, in a table of its own.

`AlwaysAttach(shard)` is the honest default for a runtime with one entry point.

## Composition

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity = Identity::self_signed(vec!["localhost".to_owned()])?;

    serve(GatewayConfig::default(), identity, |runtime| {
        let lobby = runtime.create_shard(|_handle| Lobby::new());
        AlwaysAttach(lobby.shard())
    })
    .await
}
```

The closure runs after the sockets are bound and before the first connection is
accepted. That window is the only place where "the initial shards exist before
anyone can be routed to one" holds.

`create_shard` takes a closure rather than a value because a logic often wants
its own `ShardHandle`, to wake itself from work it starts. The closure receives
that handle.

`Gateway::bind` followed by `Gateway::serve` is available when the address needs
inspecting between the two, as when binding an ephemeral port in a test.

For a worked example using two shards, an external update task and a real
router, see [Running the arena](arena.md).
