#!/usr/bin/env bash
#
# cargo-gate.sh — exécute une commande cargo uniquement si le workspace a au
# moins un membre. Sur un workspace vide (Phase 0, R5), cargo refuse de tourner
# ("manifest is virtual, workspace has no members") ; ce wrapper transforme ce
# cas en no-op vert, ce qui garde la CI verte sur un workspace vide sans avoir à
# inventer un crate factice.
#
# Usage : ci/cargo-gate.sh cargo <sous-commande> [args...]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

count=0
if command -v jq >/dev/null 2>&1; then
  count="$(cargo metadata --format-version 1 --no-deps 2>/dev/null | jq '.packages | length' 2>/dev/null || echo 0)"
fi

if [ "${count:-0}" -eq 0 ]; then
  echo "workspace vide (0 crate) — étape ignorée : $*"
  exit 0
fi

exec "$@"
