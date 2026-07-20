#!/usr/bin/env bash
#
# verifier-boundary.sh — enforce la séparation implémenteur / vérificateur (R2).
#
# Les zones vérificateur (conformance/, fixtures/, voxloom-testkit/) ne peuvent
# pas être modifiées dans le même diff qu'une implémentation (voxloom-*/src, hors
# testkit). Toute modification d'un test de conformité passe par une revue humaine.
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
    conformance/*|fixtures/*|voxloom-testkit/*)
      touches_verifier=1 ;;
    voxloom-*/src/*)
      # le testkit est une zone vérificateur, déjà couverte au-dessus.
      case "$f" in voxloom-testkit/*) ;; *) touches_impl=1 ;; esac ;;
  esac
done <<< "$CHANGED"

if [ "$touches_verifier" -eq 1 ] && [ "$touches_impl" -eq 1 ]; then
  echo "✗ verifier-boundary.sh : un même diff touche à la fois une zone"
  echo "  vérificateur (conformance/ | fixtures/ | voxloom-testkit/) et une"
  echo "  implémentation (voxloom-*/src). Interdit par R2 — séparer les diffs."
  echo "  Fichiers :"
  echo "$CHANGED" | sed 's/^/    /'
  exit 1
fi

echo "✓ verifier-boundary.sh : séparation implémenteur/vérificateur respectée."
exit 0
