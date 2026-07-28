# The render function

`render` describes what the voice session should look like now, given the
application's current state. It is called after a wake, at most once every
50 ms, and it builds a whole desired state rather than a change to one.

```rust
fn render(&mut self, out: &mut ShardBuilder<'_>);
```

Nothing is emitted from inside it. The difference against what each connection
currently holds is computed afterwards, by the runtime. See
[Deltas and per-connection composition](../model/deltas.md).

There is no viewer parameter. What is shared cannot depend on who is looking,
and what one connection alone sees is declared separately, as an overlay.

## The tree

```rust
let root = out.root("The Arena");
let red = out.channel(root, RED_BASE, "Red Base", Narrow::Into(1));
let alice = out.user(red, Occupant::Connection(who), "alice", Narrow::Same);
```

A channel demands a parent and a user demands a channel. In both cases the scope
is derived from the parent's through `Narrow`, which either keeps it or extends
it by one segment:

```rust
pub enum Narrow {
    Same,
    Into(u32),
}
```

There is no free scope parameter anywhere, and no `Widen` variant. That absence
is the whole safety argument. A user always ends up at or below its channel's
scope, so any observer seeing the user also sees the channel. The closure
property becomes something to rely on rather than something to check. See
[The shared view and scopes](../model/scopes.md).

`root` is called exactly once per render. Calling it twice is an error, because
a shard renders one tree.

## Identity is stated, not derived

`ShardBuilder::channel` asks for a `ChannelKey`. The diff has to recognise the
same channel across two renders, and an identity that drifts burns a wire
identifier every turn.

Deriving identity from the name is the obvious guess and the trap. A channel
whose name carries a clock would be destroyed and recreated ten times a second.
The key is therefore stated explicitly and the name stays a field:

```rust
const RED_BASE: ChannelKey = ChannelKey(10);

let name = format!("Red Base ({} players)", red.len());
let red = out.channel(root, RED_BASE, &name, Narrow::Into(1));
```

Rendering the same key twice in one turn is an error, not a merge.

`ActionKey` follows the same rule for the same reason: the wire identifier is a
string the client stores and echoes back, so an application using the label as
the identity would break its own buttons the day it renames one.

Users need no separate key. `Occupant` is the identity.

## Channel attributes

| call | effect |
| --- | --- |
| `channel_position` | ordering hint sent to the client |
| `channel_can_enter` | whether the client offers the channel as enterable |
| `channel_can_text` | whether the client offers a chat box for it |
| `channel_link` | symmetric link between two channels |
| `user_flags` | mute, deafen, suppress, priority speaker, recording |

`can_enter` is a display hint. An actual join arrives as an event and is
validated again then. Declaring `can_text` false greys the chat box out rather
than offering a box whose answer is a denial, and a message aimed there is
refused anyway.

A link is the one relation that does not follow the parent hierarchy, which
makes it the only structural check in the model. Linking two channels at
non-comparable scopes is refused, since a link pointing at a channel the viewer
cannot see has no meaning.

## Private elements

Elements visible to one connection are declared inside a `private` block:

```rust
out.private(admin, |private| {
    let room = private.channel(root, overwatch_key, "Overwatch");
    private.user_in(room, Occupant::Connection(admin), "admin (vanished)");
    private.action(JOIN, "Enter the Arena", On::SERVER);
});
```

A private element is never scope-filtered. Its visibility is the block it was
declared in.

Overlays are recomputed at every render and never journalled, so an element is
withdrawn by not declaring it again. A context action works the same way: the
difference against what the connection was already offered is what travels, and
no message is emitted by the application.

Whether a distinction belongs in a scope or in an overlay is the subject of
[Scope or overlay](scope-or-overlay.md).

## Audio

Three calls, and the relation they build is directed and independent of both
trees:

```rust
out.audio_domain(RED_VOICE, &red_members);  // symmetric group
out.audio_listen(spectator, RED_VOICE);     // one-way
out.audio_edge(admin, member);              // a single directed edge
```

See [Declaring audio](audio.md).

## What closes the render

Three properties are not enforced by the builder's shape and are checked when
the render closes:

- **Shared or private, never both.** An element in the shared view and in an
  overlay would drift: the next shared delta would move it without the overlay
  reasserting itself, and the client would diverge silently.
- **An overlay references only what its connection already sees.** One lookup
  per overlay element forbids, in one stroke, placing someone in a channel about
  to vanish, in a channel that connection cannot see, or referencing a session
  that does not exist.
- **A receiver sees its sender.** The Mumble client discards audio whose sender
  session it does not know, so an edge into a blind receiver is silence with
  extra steps. See [Seeing the speaker](../model/coupling.md).

The failures a render can report:

| error | cause |
| --- | --- |
| `MissingRoot`, `DuplicateRoot` | no tree, or two |
| `DuplicateChannelKey`, `DuplicateOccupant` | the same identity rendered twice |
| `ScopeTooDeep` | more than four segments |
| `LinkAcrossScopes` | a link between non-comparable scopes |
| `SharedAndPrivateUser`, `SharedAndPrivateChannel` | the first property above |
| `OverlayChannelMissing`, `OverlayChannelInvisible` | the second |
| `ReceiverCannotSeeSender` | the third |
| `TooManyActions` | more than 64 offered to one connection |
| `Exhausted` | no channel or session identifier left |

A refused render keeps the previous view and closes nothing. The committed view
is still correct, and reconnecting would only reproduce the same broken render.
