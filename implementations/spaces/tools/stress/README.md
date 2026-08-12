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
