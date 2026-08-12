#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS="$(mktemp -d)"
VOICE="$RESULTS/smoke.opuspack"
cleanup() {
  rm -r -- "$RESULTS"
}
trap cleanup EXIT

cd "$ROOT"
if base64 --help 2>&1 | grep -q -- '--decode'; then
  base64 --decode <implementations/spaces/tools/stress/fixtures/smoke.opuspack.base64 >"$VOICE"
else
  base64 -D <implementations/spaces/tools/stress/fixtures/smoke.opuspack.base64 >"$VOICE"
fi
test "$(wc -c <"$VOICE" | tr -d ' ')" -eq 630
GRADLE_USER_HOME=.gradle ./gradlew \
  :implementations:spaces:tools:load-driver-java:installDist

run_case() {
  local scenario="$1"
  shift
  cargo run --quiet -p mumble-spaces-stress --features load-metrics -- run \
    --mode managed \
    --controllers 2 \
    --participants 8 \
    --participants-per-space 8 \
    --scenario "$scenario" \
    --duration 2s \
    --result-root "$RESULTS/$scenario" \
    "$@"
}

run_case migration
run_case mute-deaf
run_case voice --voice-file "$VOICE"
ci/spaces-resilience-smoke.sh

test "$(find "$RESULTS" -name summary.json -type f | wc -l | tr -d ' ')" -eq 3
test "$(find "$RESULTS" -name 'worker-*-report.json' -type f | wc -l | tr -d ' ')" -eq 3
if find "$RESULTS" -name summary.json -type f -exec grep -L '"errors": 0' {} + | grep -q .; then
  echo "spaces-stress-smoke: a fixed scenario reported driver errors" >&2
  exit 1
fi
if find "$RESULTS" -name summary.json -type f -exec grep -L '"missing_reports": 0' {} + | grep -q .; then
  echo "spaces-stress-smoke: a fixed scenario lost a Mumble worker report" >&2
  exit 1
fi
if find "$RESULTS" -name summary.json -type f -exec grep -L '"clients_completed": 8' {} + | grep -q .; then
  echo "spaces-stress-smoke: a fixed scenario did not complete all Mumble clients" >&2
  exit 1
fi
if find "$RESULTS" -name process-metrics.csv -type f -exec grep -L ',server,' {} + | grep -q .; then
  echo "spaces-stress-smoke: managed server process metrics are missing" >&2
  exit 1
fi
if find "$RESULTS" -name process-metrics.csv -type f -exec grep -L ',coordinator,' {} + | grep -q .; then
  echo "spaces-stress-smoke: coordinator process metrics are missing" >&2
  exit 1
fi
grep -R -q '"channel":"load-space-0-migrated"' "$RESULTS/migration"
grep -R -q -E '"mute":true|"deaf":true' "$RESULTS/mute-deaf"
grep -q -E '"audio_ingress_packets":[1-9][0-9]*' \
  "$(find "$RESULTS/voice" -name server-metrics.jsonl -type f -print -quit)"
grep -q -E '"audio_egress_packets":[1-9][0-9]*' \
  "$(find "$RESULTS/voice" -name server-metrics.jsonl -type f -print -quit)"
if grep -R -E '"credential"|secret-token|bearer' "$RESULTS"; then
  echo "spaces-stress-smoke: secret-shaped data found in artifacts" >&2
  exit 1
fi

echo "spaces-stress-smoke: real migration, mute/deaf, Opus, rotation, and recovery passed"
