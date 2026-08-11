# mumble-server-runtime-stress

A headless Mumble load generator. Each simulated client completes the real
handshake over TLS, then holds the connection and optionally sends voice.

TLS certificates are deliberately not verified. Use it only against a server
authorized for load testing.

```sh
cargo run --release -p mumble-server-runtime-stress -- --clients 200 --duration 30s
```

The default target is `127.0.0.1:64738`. Raise the demo's admission ceiling
before pointing the tool at it:

```sh
cargo run -p mumble-server-runtime-arena -- 127.0.0.1:64738 500
```

Two workloads are available through `--scenario`. `connect` completes the
protocol and holds the connection open. `arena` chooses a team, migrates,
switches roles and returns to the lobby, exercising handover under load.

Client starts spread over `--ramp`, and `--duration` counts from the end of the
ramp. `--failure-threshold` sets the fraction of failed clients tolerated before
the exit code turns non-zero. `--help` lists every flag with its default.

## Voice

Voice is opt-in, enabled by `--voice-file`. The file is a raw concatenation of
fixed-size Opus packets, `--voice-frame-bytes` each. Prepare a ten-second mono
clip of 10 ms CBR packets:

```sh
runtime/tools/stress/prepare-opus.sh \
  'https://www.youtube.com/watch?v=cE0wfjsybIQ' \
  74 \
  10 \
  runtime/tools/stress/audio/crab-rave-74s.opuspack
```

Then run with voice enabled:

```sh
cargo run --release -p mumble-server-runtime-stress -- \
  --clients 200 \
  --duration 30s \
  --voice-file runtime/tools/stress/audio/crab-rave-74s.opuspack \
  --talk-percent 5 \
  --talk-spurt 2s
```

`--talk-percent` is the average share of time each client talks, in bursts of
`--talk-spurt`.

Generated media is excluded from version control. Only use audio that may
lawfully be downloaded and replayed.
