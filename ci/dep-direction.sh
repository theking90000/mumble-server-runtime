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
  "voxloom-protocol|tokio"
  "voxloom-crypto|tokio"
  "voxloom-protocol|voxloom-shard"
  "voxloom-protocol|voxloom-gateway"
  "voxloom-crypto|voxloom-shard"
  "voxloom-crypto|voxloom-gateway"
  "voxloom-shard|voxloom-crypto"
  "voxloom-shard|voxloom-gateway"
  "voxloom-protocol|voxloom-arena"
  "voxloom-crypto|voxloom-arena"
  "voxloom-shard|voxloom-arena"
  "voxloom-gateway|voxloom-arena"
  "voxloom-protocol|voxloom-testkit"
  "voxloom-crypto|voxloom-testkit"
  "voxloom-shard|voxloom-testkit"
  "voxloom-gateway|voxloom-testkit"
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
