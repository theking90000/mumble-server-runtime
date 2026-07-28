# Client interactions

Actions taken in the client are reported to the application through `observe`:

```rust
fn observe(&mut self, event: &VoiceEvent, out: &mut Reply);
```

None of them changes any voice state on arrival.
[Refusals](../model/refusals.md) explains why. This page covers handling them.

## Two doors

`render` states what **is**. `Reply` states what is **said**. That split is why
the trait has three methods rather than one per feature.

A state belongs to the render, which restates it every turn until it stops being
true. A word is said once, at a date, and no later render can restate it.

## What arrives

`VoiceEvent` is non-exhaustive, so a match on it needs a fallback arm.

| event | meaning |
| --- | --- |
| `Connected` | a connection was attached to this shard |
| `Disconnected` | gone, with a reason. No further event carries it |
| `Migrated` | left for another shard. The socket and session are intact |
| `RequestedChannel` | a channel was double-clicked or dragged into |
| `RequestedSelfState` | the client asked to mute or deafen itself |
| `InvokedAction` | a context action offered to this connection was invoked |
| `Said` | text was typed and aimed somewhere |

`Disconnected` and `Migrated` mean opposite things and are therefore separate. A
disconnect frees a slot, a migration hands it over.

## Everything arrives resolved

Events carry the application's own vocabulary, `ChannelKey` and `Occupant`,
never the wire identifiers a client holds.

They are also pre-validated. The channel named by `RequestedChannel` is one that
connection can actually see. The key in `InvokedAction` was offered to it, the
target is visible to it, and the place matches the declared bits. The audience
in `Said` is resolved against what the sender sees, and the channel it names was
rendered as writable.

What remains is entirely the application's decision, up to and including doing
nothing.

## Answering

| verb | effect |
| --- | --- |
| `say(to, text)` | tell one connection something, in the server's name |
| `refuse(to, reason)` | a refusal, shown where the client reports denials |
| `relay(from, to, text)` | deliver text to an audience, attributed to a sender |
| `announce(to, text)` | the same, in the server's name |
| `switch(connection, shard)` | hand the connection to another shard |

An `Audience` is `Channel(key)`, `Tree(key)` for a channel and everything below
it, or `User(occupant)`.

A `Reply` accumulates and is drained once `observe` has returned, so a test can
drive `observe` with a scratch reply and read back what came out.

## Accepting a request

Accepting means changing application state. Nothing else:

```rust
VoiceEvent::RequestedChannel { connection, channel } => {
    match *channel {
        RED => self.chosen.set(*connection, Intent::Join(Side::Red)),
        BLUE => self.chosen.set(*connection, Intent::Join(Side::Blue)),
        _ => {}
    }
}
```

The next render carries the consequence. No wake is needed: handling an event
already marks the shard for reconciliation.

## Refusing

Refusing means rendering nothing new. Saying so is separate, and worth doing:

```rust
VoiceEvent::InvokedAction { connection, action, .. } if *action == JOIN => {
    match self.chosen.get(*connection) {
        Some(_) => out.say(*connection, "Entering the arena."),
        None => out.refuse(*connection, "Choose a side first."),
    }
}
```

A silent refusal leaves someone pressing a button that does nothing, which is
the state `refuse` exists to end.

## Text

`Said` delivers nothing by itself. `relay` is the one line that carries it out,
and the application stays in charge of the audience. Relaying somewhere other
than where a message was aimed is a rewrite rather than a workaround.

```rust
VoiceEvent::Said { connection, to, text } => {
    out.relay(*connection, *to, text);
}
```

Two rules are applied when the audience is expanded, both borrowed from
elsewhere in the model. The sender never receives its own message. A recipient
who cannot see the sender is skipped, which is the audio coupling rule applied
to text.

`announce` has no actor, so nobody is skipped for not seeing one. It is the
right verb for something everyone should read whoever said it.

## Self-mute and self-deafen

`RequestedSelfState` carries two `Option<bool>`. `None` means the client said
nothing about that flag, so whatever is currently rendered stands.

The runtime keeps no copy of the pair. An application granting the request
stores it and hands it back through `user_flags` on the next render. Keeping a
second copy in the runtime is how the two start disagreeing.

## Moving a connection

```rust
out.switch(connection, arena);
```

Anything said in the same breath is delivered first, so a farewell reaches the
socket before the move is asked for. See
[Migration between shards](migration.md).
