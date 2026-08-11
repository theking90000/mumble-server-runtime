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

assert_controller_gate_rejects() {
  local layout="$1"
  local source_file="$2"
  local forbidden_source="$3"
  local fixture="$TEMP_ROOT/controller-$layout"

  mkdir -p "$fixture/ci" "$fixture/$(dirname "$source_file")"
  cp "$ROOT/ci/gates.sh" "$fixture/ci/gates.sh"
  printf '%s\n' "$forbidden_source" > "$fixture/$source_file"

  if (cd "$fixture" && ci/gates.sh >/dev/null 2>&1); then
    echo "structural-gates-self-test.sh: gates.sh accepted $layout Controller dependency" >&2
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

assert_dependency_rejects() {
  local layout="$1"
  local from_package="$2"
  local to_package="$3"
  local fixture="$TEMP_ROOT/dependency-$layout"

  command -v jq >/dev/null 2>&1 || return 0
  mkdir -p "$fixture/ci" "$fixture/from/src" "$fixture/to/src"
  cp "$ROOT/ci/dep-direction.sh" "$fixture/ci/dep-direction.sh"
  printf '%s\n' '[workspace]' 'members = ["from", "to"]' 'resolver = "3"' > "$fixture/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    "name = \"$from_package\"" \
    'version = "0.0.0"' \
    'edition = "2024"' \
    '' \
    '[dependencies]' \
    "$to_package = { path = \"../to\" }" > "$fixture/from/Cargo.toml"
  printf '%s\n' \
    '[package]' \
    "name = \"$to_package\"" \
    'version = "0.0.0"' \
    'edition = "2024"' > "$fixture/to/Cargo.toml"
  printf '%s\n' 'pub fn marker() {}' > "$fixture/from/src/lib.rs"
  printf '%s\n' 'pub fn marker() {}' > "$fixture/to/src/lib.rs"

  if (cd "$fixture" && ci/dep-direction.sh >/dev/null 2>&1); then
    echo "structural-gates-self-test.sh: dependency gate accepted $layout edge" >&2
    exit 1
  fi
}

assert_gate_rejects new runtime/crates/shard
assert_boundary_rejects new runtime/crates/shard/src/lib.rs runtime/verification/fixtures/corpus.bin
assert_controller_gate_rejects core-runtime control-plane/core/rust/src/controller.rs \
  'use mumble_server_runtime_gateway::RuntimeHandle;'
assert_controller_gate_rejects core-spaces control-plane/core/rust/src/controller.rs \
  'struct SpaceSnapshot;'
assert_controller_gate_rejects host-spaces control-plane/host/rust/src/host.rs \
  'use mumble_controller_spaces::SpaceKey;'
assert_dependency_rejects core-runtime \
  mumble-controller-core \
  mumble-server-runtime-gateway
assert_dependency_rejects host-spaces \
  mumble-controller-host \
  mumble-controller-spaces
assert_boundary_rejects controller \
  control-plane/core/rust/src/controller.rs \
  control-plane/verification/core-conformance/tests/session.rs

echo "✓ structural-gates-self-test.sh: runtime and Controller boundaries are enforced."
