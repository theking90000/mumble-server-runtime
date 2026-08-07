# Spaces

A Space is a place where people hear each other. Putting two participants in
the same Space connects them.

## Choosing keys

A `SpaceKey` is a string you choose:

```java
SpaceKey.of("lobby")
SpaceKey.of("team-red")
SpaceKey.of("arena-3")
SpaceKey.of("world:overworld/region:12,-4")
```

Choose a key that can be computed from your own state. Placing a player then
becomes a function, and the rest of your code never has to remember where
anyone is:

```java
private SpaceKey spaceOf(Player player) {
    Team team = teamOf(player);
    return team == null
            ? SpaceKey.of("lobby")
            : SpaceKey.of("team-" + team.getName());
}
```

## Lifetime

A Space starts existing when the first participant is placed in it, and stops
existing shortly after the last one leaves. The default delay is thirty
seconds, so that passing briefly through an empty Space does not destroy and
rebuild it. There is no create call and no delete call.

Reusing a key after its Space has closed produces a new Space, not the old one
reopened. It carries a different `SpaceIncarnation`, which prevents a stale
snapshot from being read as a current one.

## Reading who is in a Space

Useful for a `/voice list` command, a scoreboard or an admin display.

A Space holding one of your own participants is always visible to you. To read
one that holds none, ask for it:

```java
session.observeSpace(SpaceKey.of("lobby"));
```

The cache can then be read at any time:

```java
SpaceSnapshot lobby = session.spaces().get(SpaceKey.of("lobby"));
if (lobby != null) {
    for (SpaceParticipant participant : lobby.participants()) {
        System.out.println(participant.displayName()
                + (participant.mumbleConnected() ? " (connected)" : " (away)"));
    }
}
```

Changes can also be pushed to you:

```java
session.addSpaceListener(new SpaceListener() {
    @Override
    public void onSpaceUpdated(ControllerSession session, SpaceSnapshot snapshot) {
        refreshScoreboard(snapshot);
    }
});
```

Every `SpaceSnapshot` is a complete replacement rather than a patch. Discard
the previous value and use the new one.

`observeSpace` and `unobserveSpace` only affect the Spaces you asked for
explicitly. Unobserving a Space that still holds one of your participants does
not stop its updates, since that Space is visible to you regardless.

## One-off reads

`fetchSpace` reads a Space once, without subscribing to it:

```java
session.fetchSpace(SpaceKey.of("arena-3"))
        .thenAccept(snapshot -> showTo(admin, snapshot));
```

Use it for a command that answers a question once. It does not touch the cache,
does not affect later updates, is never retried automatically, and fails if the
connection drops before the answer arrives. It also requires an active session,
unlike `observeSpace`, which can be declared at any time.

Use `observeSpace` when the answer stays on screen, and `fetchSpace` when it is
printed once.

## What a snapshot contains

`SpaceSnapshot` describes one Space at one instant:

| Method | Meaning |
|---|---|
| `spaceKey()` | The key you asked for |
| `participants()` | Everyone in it, including participants owned by other applications |
| `incarnation()` | Which materialization of this key this is |
| `spaceRevision()` | Version counter within that incarnation |
| `publishedGeneration()` | Which Mumble generation this reflects |

Each `SpaceParticipant` provides `participantId()`, `displayName()`,
`serverMute()`, `serverDeaf()` and `mumbleConnected()`.

A participant you registered whose player has not opened Mumble yet appears in
the snapshot with `mumbleConnected()` false. They are declared but not present
in voice.

To order two snapshots of the same key, compare `incarnation()` first and
`spaceRevision()` only if the incarnations match. Revisions from different
incarnations are unrelated numbers.

## Sharing a Space between applications

Several controller sessions can place participants in the same Space, and those
participants hear each other. Each session owns only the participants it
registered: it sees the others in a snapshot, but cannot move, mute or remove
them.

Two plugins owning different players and agreeing on a key convention therefore
work together without coordinating with each other.
