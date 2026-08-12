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
  --scenario migration \
  --duration 2s \
  --result-root "$RESULTS"

SUMMARY="$(find "$RESULTS" -name summary.json -type f -print -quit)"
test -n "$SUMMARY"
grep -q '"errors": 0' "$SUMMARY"
if grep -R -E '"credential"|secret-token|bearer' "$RESULTS"; then
  echo "spaces-stress-smoke: secret-shaped data found in artifacts" >&2
  exit 1
fi

echo "spaces-stress-smoke: Java -> gRPC -> Rust -> Mumble migration passed"
