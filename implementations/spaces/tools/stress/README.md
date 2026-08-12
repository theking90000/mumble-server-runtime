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

Generate a standalone HTML report from an existing run without repeating it:

```sh
python3 implementations/spaces/tools/stress/report.py \
  /var/tmp/mumble-spaces-load-runs/1786558655-Idle-42
```

This writes `report.html` in the run directory. Passing the result root instead
generates one report per run plus a comparative `index.html`:

```sh
python3 implementations/spaces/tools/stress/report.py \
  /var/tmp/mumble-spaces-load-runs
```

The generator uses only the Python standard library and embeds its charts and
aggregated measurements directly in the HTML. It does not embed raw logs or
unknown JSON fields. Incomplete runs are reported too, including recognized OS
resource failures such as file-descriptor exhaustion.

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

## Linux network chaos

`ci/spaces-chaos-linux.sh` creates one Linux network namespace per Java driver
and per Mumble worker. It supports one-way or two-way gRPC partitions,
latency/jitter, bandwidth limitation, Mumble UDP loss without cutting TCP,
Mumble TCP cuts without deleting UDP, and a partition during takeover.

Inspect the complete operation list without privileges or side effects:

```sh
ci/spaces-chaos-linux.sh --dry-run --fault takeover-partition
```

A real run needs Linux, root, `iproute2`, `tc`, and `nftables`, and deliberately
requires the acknowledgement flag:

```sh
sudo ci/spaces-chaos-linux.sh \
  --smoke \
  --ack-dedicated-host \
  --fault takeover-partition \
  --result-root /var/tmp/voxloom-chaos
```

Run this only on a dedicated machine explicitly authorized to suffer network
partitions and resource pressure. The cleanup trap removes namespaces, veth
pairs, bridge, nft rules, and qdiscs. The smoke collects process threads, file
descriptors, and sockets from `/proc`, then requires takeover convergence and
credential-redaction before succeeding. `--managed-bind-ip` acknowledges that
the private Controller endpoint is plaintext and must remain on the isolated
test bridge.

The committed smoke fixture is a short 440 Hz synthetic tone encoded as 10 ms,
24 kbit/s CBR Opus packets and stored as base64 so the smoke does not require
FFmpeg or third-party media at runtime.
