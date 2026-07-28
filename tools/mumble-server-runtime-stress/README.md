# mumble-server-runtime-stress audio

Voice is opt-in. Prepare a ten-second, mono Opus clip made of 10 ms CBR packets:

```sh
tools/mumble-server-runtime-stress/prepare-opus.sh \
  'https://www.youtube.com/watch?v=cE0wfjsybIQ' \
  74 \
  10 \
  tools/mumble-server-runtime-stress/audio/crab-rave-74s.opuspack
```

Then run the load generator:

```sh
cargo run --release -p mumble-server-runtime-stress -- \
  --clients 200 \
  --duration 30s \
  --voice-file tools/mumble-server-runtime-stress/audio/crab-rave-74s.opuspack \
  --talk-percent 5 \
  --talk-spurt 2s
```

The generated media is deliberately ignored by Git. Only use audio that you are
allowed to download and replay.
