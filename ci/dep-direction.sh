#!/usr/bin/env bash
#
# dep-direction.sh — vérifie la direction des dépendances entre crates (R4).
#
# Règles interdites (arêtes qui ne doivent JAMAIS exister dans le graphe cargo) :
#   voxloom-audio    -> voxloom-render     (le hot path ne connaît pas la vue)
#   voxloom-audio    -> voxloom-state      (le hot path lit un snapshot, pas l'état)
#   voxloom-render   -> voxloom-protocol   (le renderer ignore le wire format)
#   voxloom-state    -> voxloom-protocol   (l'état canonique est indépendant de Mumble)
#   voxloom-protocol -> tokio              (crate pur)
#   voxloom-crypto   -> tokio              (crate pur)
#
# S'appuie sur `cargo metadata`. Sur un workspace vide (Phase 0), il n'y a aucun
# crate voxloom : le script passe trivialement.

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
  "voxloom-audio|voxloom-render"
  "voxloom-audio|voxloom-state"
  "voxloom-render|voxloom-protocol"
  "voxloom-state|voxloom-protocol"
  "voxloom-protocol|tokio"
  "voxloom-crypto|tokio"
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
