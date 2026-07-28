#!/usr/bin/env bash
#
# fuzz-smoke.sh — short cargo-fuzz run over every target (Task 6).
#
# A smoke run: proves each target builds and finds no crash within a small time
# budget. Long soak runs belong in a nightly job. Requires a nightly toolchain
# and cargo-fuzz (`cargo install cargo-fuzz`); on a stable-only host it skips
# with a clear message rather than failing.
#
# Usage: ci/fuzz-smoke.sh [seconds_per_target]   (default 30)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SECONDS_PER_TARGET="${1:-30}"
TARGETS=(framing control udp ocb2_decrypt control_roundtrip udp_roundtrip)

if ! rustup run nightly rustc --version >/dev/null 2>&1; then
  echo "fuzz-smoke.sh : nightly toolchain absent — gate ignoré (rustup toolchain install nightly)."
  exit 0
fi
if ! cargo +nightly fuzz --version >/dev/null 2>&1; then
  echo "fuzz-smoke.sh : cargo-fuzz absent — gate ignoré (cargo install cargo-fuzz)."
  exit 0
fi

for target in "${TARGETS[@]}"; do
  echo "== fuzzing $target for ${SECONDS_PER_TARGET}s =="
  cargo +nightly fuzz run "$target" -- -max_total_time="$SECONDS_PER_TARGET"
done

echo "✓ fuzz-smoke.sh : aucun crash sur ${#TARGETS[@]} cible(s)."
