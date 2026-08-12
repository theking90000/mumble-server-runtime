#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS="$(mktemp -d)"
cleanup() {
  rm -r -- "$RESULTS"
}
trap cleanup EXIT

cd "$ROOT"
GRADLE_USER_HOME=.gradle ./gradlew \
  :implementations:spaces:tools:load-driver-java:installDist
cargo run --quiet -p mumble-spaces-stress -- run \
  --mode managed \
  --controllers 2 \
  --participants 8 \
  --participants-per-space 8 \
  --scenario batch-takeover \
  --duration 2s \
  --result-root "$RESULTS"

SUMMARY="$(find "$RESULTS" -name summary.json -type f -print -quit)"
EVENTS="$(find "$RESULTS" -name events.jsonl -type f -print -quit)"
PROCESSES="$(find "$RESULTS" -name process-metrics.csv -type f -print -quit)"
test -n "$SUMMARY"
test -n "$EVENTS"
test -n "$PROCESSES"
grep -q '"credential_rotations": 16' "$SUMMARY"
grep -q '"ownership_violations": 0' "$SUMMARY"
grep -q '"driver_processes": 2' "$SUMMARY"
grep -q '"phase":"injection"' "$EVENTS"
grep -q '"phase":"final_audit"' "$EVENTS"
grep -q ',server,' "$PROCESSES"
grep -q ',worker-1,' "$PROCESSES"
if grep -R -E '"credential"|secret-token|bearer' "$RESULTS"; then
  echo "spaces-resilience-smoke: secret-shaped data found in artifacts" >&2
  exit 1
fi

echo "spaces-resilience-smoke: 2 Controllers / 8 participants takeover passed"
