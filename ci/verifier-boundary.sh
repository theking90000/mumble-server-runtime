#!/usr/bin/env bash
#
# verifier-boundary.sh — enforce la séparation implémenteur / vérificateur (R2).
#
# Les zones vérificateur ne peuvent pas être modifiées dans le même diff que les
# sources d'une implémentation.
# Toute modification d'un test de conformité passe par une revue humaine.
#
# Usage :
#   ci/verifier-boundary.sh <base_ref>     # compare HEAD à <base_ref>
#   ci/verifier-boundary.sh                # compare au parent (HEAD~1) si dispo

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BASE="${1:-}"
if [ -z "$BASE" ]; then
  if git rev-parse --verify -q HEAD~1 >/dev/null; then
    BASE="HEAD~1"
  else
    echo "verifier-boundary.sh : pas de base de comparaison, gate ignoré."
    exit 0
  fi
fi

CHANGED="$(git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only "$BASE" || true)"
[ -n "$CHANGED" ] || { echo "verifier-boundary.sh : aucun fichier modifié."; exit 0; }

touches_verifier=0
touches_impl=0

while IFS= read -r f; do
  [ -n "$f" ] || continue
  case "$f" in
    conformance/*|runtime/verification/testkit/*|runtime/verification/fixtures/*|control/coordination/verification/*|implementations/spaces/verification/*)
      touches_verifier=1 ;;
    runtime/crates/*/src/*|control/coordination/protocol/*|control/coordination/rust/src/*|control/coordination/sdk/*|control/runtime-adapter/rust/src/*|implementations/spaces/protocol/*|implementations/spaces/rust/src/*|implementations/spaces/sdk/*|implementations/spaces/host/rust/src/*|implementations/spaces/bukkit/*)
      touches_impl=1 ;;
  esac
done <<< "$CHANGED"

if [ "$touches_verifier" -eq 1 ] && [ "$touches_impl" -eq 1 ]; then
  echo "✗ verifier-boundary.sh : un même diff touche à la fois une zone"
  echo "  vérificateur et une implémentation. Interdit par R2 — séparer les diffs."
  echo "  Fichiers :"
  echo "$CHANGED" | sed 's/^/    /'
  exit 1
fi

echo "✓ verifier-boundary.sh : séparation implémenteur/vérificateur respectée."
exit 0
