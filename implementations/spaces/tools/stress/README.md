# Spaces headless stress harness

This tool measures the Java Spaces SDK, the Rust Spaces host, and real headless
Mumble TLS/TCP/UDP clients without starting Minecraft or Bukkit.

Build the Java driver once, then run a managed eight-client smoke:

```sh
GRADLE_USER_HOME=.gradle ./gradlew \
  :implementations:spaces:tools:load-driver-java:installDist
cargo run --release -p mumble-spaces-stress -- run \
  --mode managed \
  --controllers 2 \
  --participants 8 \
  --participants-per-space 8 \
  --scenario migration \
  --duration 30s
```

`--mode external` requires both `--controller-endpoint` and `--mumble-server`.
The participant count must fill every Space exactly; supported cardinalities are
8, 32, 64, 128, and 256. A run writes a manifest, redacted events, JSON and CSV
summaries, and separate process logs below `--result-root`.
Managed runs also write one-second `server-metrics.jsonl` snapshots and a
`process-metrics.csv` that keeps coordinator, Java driver, and Mumble worker
resource usage separate. The standalone server exposes the same opt-in stream
through `--metrics-output FILE --metrics-interval-seconds N`.

The harness is not a conformance oracle. Keep the independent Core and Spaces
verification suites enabled when interpreting load results.

## Resilience campaigns

The named multi-Controller scenarios are `multi-shared-spaces`,
`multi-isolated-spaces`, `skew-90-10`, `batch-takeover`,
`simultaneous-claim`, `crossed-takeover`, `ping-pong`, `stale-reconnect`,
`identity-boundary`, and `overload-recovery`. They require at least two
Controller IDs. Takeovers register the same participant through another real
Java SDK process; the harness does not invent a takeover API.

Every injected fault emits the fixed phases `stable`, `injection`,
`degraded_load`, `healing`, `reconciliation`, and `final_audit`. The optional
`--fault` selects `graceful-stop`, `crash`, `freeze-short`, `freeze-long`,
`host-restart`, `mumble-cut`, or `controlled-limit`. `stale-reconnect`,
`identity-boundary`, and `overload-recovery` select their corresponding fault
when `--fault none` is left in place. Freeze faults require Unix process
signals. A host restart means all independently hosted Java Controller
processes are replaced; it does not restart the Spaces server.

The final audit checks the expected owner, credential rotation without token
reuse, revision observations, and recovery of unaffected sessions. Core
conformance remains the authority for fencing semantics.

Generate the exact full-Space load matrix without running the long campaign:

```sh
cargo run -p mumble-spaces-stress -- matrix --output spaces-load-matrix.json
```

It contains cardinalities 8, 32, 64, 128, and 256, the rounded load steps,
fixed totals 2,048 and 4,096, three audio levels, resilience fractions, and the
provisional health thresholds. The 10,000-client campaign is intended for a
dedicated Linux load host, not shared CI.
