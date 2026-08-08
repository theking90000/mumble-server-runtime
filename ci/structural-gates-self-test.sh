#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEMP_ROOT"' EXIT

assert_gate_rejects() {
  local layout="$1"
  local shard_dir="$2"
  local fixture="$TEMP_ROOT/$layout"

  mkdir -p "$fixture/ci" "$fixture/$shard_dir/src"
  cp "$ROOT/ci/gates.sh" "$fixture/ci/gates.sh"
  printf '%s\n' 'use std::net::TcpStream;' > "$fixture/$shard_dir/src/forbidden.rs"

  if (cd "$fixture" && ci/gates.sh >/dev/null 2>&1); then
    echo "structural-gates-self-test.sh: gates.sh accepted $layout shard networking" >&2
    exit 1
  fi
}

assert_boundary_rejects() {
  local layout="$1"
  local implementation_file="$2"
  local verifier_file="$3"
  local fixture="$TEMP_ROOT/boundary-$layout"

  mkdir -p "$fixture/ci" "$fixture/$(dirname "$implementation_file")" "$fixture/$(dirname "$verifier_file")"
  cp "$ROOT/ci/verifier-boundary.sh" "$fixture/ci/verifier-boundary.sh"
  git -C "$fixture" init -q
  git -C "$fixture" config user.email self-test@example.invalid
  git -C "$fixture" config user.name structural-gates-self-test
  touch "$fixture/$implementation_file" "$fixture/$verifier_file"
  git -C "$fixture" add .
  git -C "$fixture" commit -qm baseline
  local base
  base="$(git -C "$fixture" rev-parse HEAD)"
  printf '%s\n' changed > "$fixture/$implementation_file"
  printf '%s\n' changed > "$fixture/$verifier_file"
  git -C "$fixture" add .
  git -C "$fixture" commit -qm mixed-change

  if (cd "$fixture" && ci/verifier-boundary.sh "$base" >/dev/null 2>&1); then
    echo "structural-gates-self-test.sh: verifier boundary accepted $layout mixed diff" >&2
    exit 1
  fi
}

assert_gate_rejects old mumble-server-runtime-shard
assert_gate_rejects new runtime/crates/shard
assert_boundary_rejects old mumble-server-runtime-shard/src/lib.rs fixtures/corpus.bin
assert_boundary_rejects new runtime/crates/shard/src/lib.rs runtime/verification/fixtures/corpus.bin

echo "✓ structural-gates-self-test.sh: old and new layouts are enforced."
