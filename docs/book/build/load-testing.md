# Load testing

Two tools, measuring two different things. `bench-shard` measures the cost of
publishing a change, with no sockets involved. `mumble-server-runtime-stress`
measures a running server through real connections.

## The cost of one turn

```console
$ cargo run --release -p bench-shard
```

A turn here is the complete cold path of a state change: render the shard once,
plan the shared delta once, journal it, republish the audio routing table, then
for every connection filter that delta, splice its overlay, collapse, encode and
push.

Draining the queues belongs to the connection tasks rather than the shard, so it
happens outside the timed section. The figure measures the publication pipeline,
not the socket.

Five connection counts are measured, 2 through 500, over twenty turns each. The
reported figure is the median, so a run under unrelated system load still
reports honestly.

```text
  conns        median  per connection         worst  delta ops
```

The last column is the one the exercise is about. It is the size of the shared
delta, and it stays small while the connection count grows. A run where it
tracks the population instead is the quadratic shape returning. See
[Cost](../model/cost.md).

The workload is a two-realm world with one member moved between realms per
generation. The initial synchronisation is not timed.

`ci/bench-shard.sh` runs the same thing and deliberately applies no time
threshold. A continuous integration machine is not a stable bench. It fails only
when a turn fails, and prints the table so changes stay visible in the log.

## Load against a running server

The stress tool opens real TLS and UDP connections and drives headless Mumble
clients. Certificates are deliberately not verified.

Raise the admission ceiling first, since the demo's default is conservative:

```console
$ cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738 500
$ cargo run --release -p mumble-server-runtime-stress -- \
    --clients 200 --duration 30s
```

Build it in release mode. A debug build measures the load generator.

| flag | default | effect |
| --- | --- | --- |
| `--server` | `127.0.0.1:64738` | TCP and UDP address |
| `-n, --clients` | 1 | concurrent clients |
| `--scenario` | `connect` | `connect` or `arena` |
| `--ramp` | `0s` | how far apart client starts are spread |
| `--duration` | `30s` | hold time after the ramp |
| `--ping-interval` | `5s` | TCP and UDP keepalive |
| `--username-prefix` | `stress` | |
| `--password` | none | opaque, reaches the router as a credential |
| `--failure-threshold` | `0` | tolerated fraction of failed clients |

Durations take `ms`, `s` and `m` suffixes.

The exit code reflects the failure threshold, which is what makes the tool
usable from a script rather than only by eye.

## The two scenarios

`connect` establishes a connection, completes the handshake and holds it. It
measures admission and the steady state.

`arena` drives the demonstration application: pick a team in the lobby, migrate
through `> Enter the Arena`, switch base, return. It is state-driven rather than
timed, so each interaction is sent only once the client's own Mumble model
proves the previous one arrived. What it exercises is the slow path, since every
one of those moves changes an observation.

## Voice

Voice is opt-in and needs a clip of raw Opus packets of a fixed size:

```console
$ cargo run --release -p mumble-server-runtime-stress -- \
    --clients 200 \
    --duration 30s \
    --voice-file audio/clip.opuspack \
    --talk-percent 5 \
    --talk-spurt 2s
```

`prepare-opus.sh` under the tool's directory builds such a clip. Generated media
is ignored by Git. Only material that may lawfully be downloaded and replayed
belongs there.

Frames are 10 ms, matching what a real client emits, and
`--voice-frame-bytes` states the fixed packet size the file is cut into.

## What comes out

Distributions are reported as p50, p95, p99 and max for TCP connect, TLS
handshake, Mumble synchronisation, and both ping round trips. Counters cover
frames, packets, voice in both directions, interactions and denials.

`denied` is worth watching in the `arena` scenario. A denial there means the
application refused an interaction, which is either correct behaviour or a
scenario out of step with the logic, and telling the two apart is the point of
reporting it separately.
