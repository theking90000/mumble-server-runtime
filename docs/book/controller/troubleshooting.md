# Troubleshooting

Common failures, and what causes them.

## The session never becomes ACTIVE

Log the state transitions before guessing:

```java
session.addSessionListener(new ControllerSessionListener() {
    @Override
    public void onStateChanged(ControllerSession session,
                               ControllerSessionState previous,
                               ControllerSessionState current) {
        getLogger().info("voice session: " + previous + " -> " + current);
    }
});
```

| What you see | What it means |
|---|---|
| Stuck in `CONNECTING` | Nothing is listening at the endpoint |
| Cycling through `RECONNECTING` | Reachable, but the stream keeps dropping |
| `FAILED` | Permanent error; the future returned by `start()` carries the cause |

Check, in order: the controller server is running, the endpoint host and port
match what it printed at startup, and the scheme is `http` rather than `https`
if you did not configure TLS.

## "controller bind ... is not loopback"

```text
controller bind 0.0.0.0:4000 is not loopback; pass
--allow-unauthenticated-controller-network to acknowledge plaintext
unauthenticated access
```

The controller port is plaintext and does not authenticate callers in v1.
Anyone able to reach it can move and mute your participants. The server
therefore refuses to bind it to a public interface unless told to explicitly.

Keep it on loopback and run the controller server on the same machine as the
game server. If it genuinely has to cross a network, pass the flag and put the
port behind a firewall or a private network.

## The player cannot connect to Mumble

**Wrong certificate or password.** The server reports one of two reasons:

- *a Mumble join token is required*: the client connected without a password.
  Passwords are not optional here, since the token is the only thing that
  identifies the participant.
- *the Mumble join token is invalid or revoked*: the token is stale. It was
  rotated by a new acquisition, or the participant was unregistered or revoked.
  Read `handle.mumbleJoinToken()` again and send the current value.

**A certificate warning.** Expected with `--dev-self-signed`, which generates a
new certificate on every start. Accept it in development, and use a real
certificate with `--mumble-cert` and `--mumble-key` elsewhere.

**Connection refused on localhost.** The Mumble listener binds IPv4
(`0.0.0.0:64738`). Some clients resolve `localhost` to `::1` first and never
try IPv4. Connect to `127.0.0.1` explicitly.

## The player connects but hears nobody

Check `appliedSpaceKey()` in both participants' status. If it differs from what
you set, the runtime could not honour your description, and `applicationError()`
says why.

If both are in the same Space and still cannot hear each other, check
`serverMute` and `serverDeaf` in your specs, then `selfMute()` and `selfDeaf()`
in their status. A player who muted themselves in their own client looks
exactly like a broken integration from the outside.

## Exceptions

| Exception | Cause | Fix |
|---|---|---|
| `IllegalStateException` from `registerParticipant` | That identifier is already registered, or a release is still in flight | Reuse `session.participant(id)`, or wait for the `unregister()` future |
| `SessionClosedException` | The session or handle is stopping or terminal | Do not reuse a stopped session or a revoked handle; both are single-use |
| `OwnershipLostException` | Another application took the participant, or the lease expired | Register again for a new handle |
| `CommandRejectedException` | The runtime refused a command; `code()` says which rule | Usually a limit, see below |
| `ControllerException` from `fetchSpace` | The session is not active at that moment | Retry once the state is `ACTIVE`, or use `observeSpace` instead |

## Limits

The server enforces resource limits and rejects commands that would exceed
them. The defaults are generous for a single game server:

| Limit | Default |
|---|---|
| Participants in total | 10,000 |
| Participants per session | 5,000 |
| Spaces | 1,024 |
| Observed Spaces per session | 1,024 |
| Concurrent Mumble connections | 100 |
| Sessions | 64 |

`--max-mumble-connections` is the first one to raise for a real deployment,
since it caps how many players can be in voice at once and 100 is a development
default. Every limit has a command-line flag, listed on the
[Rust server](server.md) page.

## Everyone was disconnected when the plugin restarted

Ownership is held by a lease, thirty seconds by default. Losing the gRPC
connection suspends the session without ejecting anyone, so a restart that
finishes within the lease leaves players talking. A restart that takes longer
lets the lease expire, which revokes the participants and closes their Mumble
connections.

Raise `--lease-seconds` if your reload cycle is slower than that.

## Nothing happens after setSpec

`setSpec` does not block and does not throw when the connection is down. It
records a new description, which is sent when the connection allows. That is
the intended behaviour, and it is why your code needs no retry logic.

To find out whether a description actually landed, wait on the returned future
or watch `onStatusChanged`. The absence of an exception says nothing.
