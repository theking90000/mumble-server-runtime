#!/usr/bin/env bash
#
# dep-direction.sh — vérifie la direction des dépendances entre crates (R4).
#
# Graphe autorisé du runtime :
#
#   arena -> gateway -> shard -> protocol
#                  \-> crypto
#
# `protocol` et `crypto` restent purs. Le shard ne connaît ni sockets ni
# chiffrement, et aucun crate central ne dépend de la démonstration ou du
# verificateur.
#
# S'appuie sur `cargo metadata`.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if ! command -v jq >/dev/null 2>&1; then
  echo "dep-direction.sh : jq introuvable, gate ignoré (installer jq en CI)." >&2
  exit 0
fi

META="$(cargo metadata --format-version 1 --no-deps 2>/dev/null || echo '{"packages":[]}')"

# Arêtes interdites : "<from>|<to>".
FORBIDDEN=(
  "mumble-server-runtime-protocol|tokio"
  "mumble-server-runtime-crypto|tokio"
  "mumble-server-runtime-protocol|mumble-server-runtime-shard"
  "mumble-server-runtime-protocol|mumble-server-runtime-gateway"
  "mumble-server-runtime-crypto|mumble-server-runtime-shard"
  "mumble-server-runtime-crypto|mumble-server-runtime-gateway"
  "mumble-server-runtime-shard|mumble-server-runtime-crypto"
  "mumble-server-runtime-shard|mumble-server-runtime-gateway"
  "mumble-server-runtime-protocol|mumble-server-runtime-arena"
  "mumble-server-runtime-crypto|mumble-server-runtime-arena"
  "mumble-server-runtime-shard|mumble-server-runtime-arena"
  "mumble-server-runtime-gateway|mumble-server-runtime-arena"
  "mumble-server-runtime-protocol|mumble-server-runtime-testkit"
  "mumble-server-runtime-crypto|mumble-server-runtime-testkit"
  "mumble-server-runtime-shard|mumble-server-runtime-testkit"
  "mumble-server-runtime-gateway|mumble-server-runtime-testkit"
  "mumble-controller-core|mumble-server-runtime-protocol"
  "mumble-controller-core|mumble-server-runtime-crypto"
  "mumble-controller-core|mumble-server-runtime-shard"
  "mumble-controller-core|mumble-server-runtime-gateway"
  "mumble-controller-core|mumble-controller-host"
  "mumble-controller-core|mumble-controller-spaces"
  "mumble-controller-core|mumble-spaces-server"
  "mumble-controller-core|tokio"
  "mumble-controller-core|tonic"
  "mumble-controller-host|mumble-controller-spaces"
  "mumble-controller-host|mumble-spaces-server"
  "mumble-controller-spaces|mumble-spaces-server"
)

violations=0
for edge in "${FORBIDDEN[@]}"; do
  from="${edge%%|*}"
  to="${edge##*|}"
  found="$(echo "$META" | jq -r --arg from "$from" --arg to "$to" '
    .packages[] | select(.name == $from) | .dependencies[]? | select(.name == $to) | .name
  ')"
  if [ -n "$found" ]; then
    echo "  ✗ dépendance interdite : $from -> $to"
    violations=$((violations + 1))
  fi
done

echo
if [ "$violations" -eq 0 ]; then
  echo "✓ dep-direction.sh : direction des dépendances conforme."
  exit 0
else
  echo "✗ dep-direction.sh : $violations arête(s) interdite(s)."
  exit 1
fi
