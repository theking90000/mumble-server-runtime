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

assert_controller_gate_ignores_build_artifact() {
  local fixture="$TEMP_ROOT/controller-build-artifact"

  mkdir -p "$fixture/ci" "$fixture/control/coordination/sdk/java/build/generated"
  cp "$ROOT/ci/gates.sh" "$fixture/ci/gates.sh"
  printf '%s\n' 'use tonic::Status;' \
    > "$fixture/control/coordination/sdk/java/build/generated/Generated.java"

  if ! (cd "$fixture" && ci/gates.sh >/dev/null 2>&1); then
    echo "structural-gates-self-test.sh: gates.sh scanned a generated build artifact" >&2
    exit 1
  fi
}

assert_legacy_controller_contract_rejected() {
  local fixture="$TEMP_ROOT/controller-legacy-contract"

  mkdir -p "$fixture/ci" \
    "$fixture/control/contract/src/main/proto/mumble/controller/v1"
  cp "$ROOT/ci/gates.sh" "$fixture/ci/gates.sh"
  printf '%s\n' 'syntax = "proto3";' \
    > "$fixture/control/contract/src/main/proto/mumble/controller/v1/controller.proto"

  if (cd "$fixture" && ci/gates.sh >/dev/null 2>&1); then
    echo "structural-gates-self-test.sh: gates.sh accepted the legacy Controller contract layer" >&2
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
assert_controller_gate_rejects coordination-runtime control/coordination/rust/src/controller.rs \
  'use mumble_server_runtime_gateway::RuntimeHandle;'
assert_controller_gate_rejects coordination-spaces control/coordination/rust/src/controller.rs \
  'struct SpaceSnapshot;'
assert_controller_gate_rejects coordination-java-spaces \
  control/coordination/sdk/java/src/main/java/example/ControllerSession.java \
  'import be.theking90000.mumble.controller.spaces.SpacesClient;'
assert_controller_gate_rejects coordination-protocol-spaces \
  control/coordination/protocol/src/main/proto/example/core.proto \
  'message FetchSpace {}'
assert_controller_gate_rejects legacy-controller-package \
  control/coordination/protocol/src/main/proto/example/core.proto \
  'package mumble.controller.v1;'
assert_legacy_controller_contract_rejected
assert_controller_gate_rejects spaces-sdk-session-engine \
  implementations/spaces/sdk/java/src/main/java/example/SpacesSession.java \
  'final class SpacesSession implements CoreTransport {}'
assert_controller_gate_ignores_build_artifact
assert_controller_gate_rejects runtime-adapter-spaces control/runtime-adapter/rust/src/host.rs \
  'use mumble_controller_spaces::SpaceKey;'
assert_dependency_rejects core-runtime \
  mumble-controller-core \
  mumble-server-runtime-gateway
assert_dependency_rejects host-spaces \
  mumble-controller-host \
  mumble-controller-spaces
assert_boundary_rejects coordination \
  control/coordination/rust/src/controller.rs \
  control/coordination/verification/core-conformance/tests/session.rs
assert_boundary_rejects coordination-java \
  control/coordination/sdk/java/src/main/java/example/ControllerSession.java \
  control/coordination/verification/core-conformance/tests/session.rs
assert_boundary_rejects spaces \
  implementations/spaces/protocol/src/main/proto/spaces.proto \
  implementations/spaces/verification/spaces-interop/scenario.rs

echo "✓ structural-gates-self-test.sh: runtime and Controller boundaries are enforced."
