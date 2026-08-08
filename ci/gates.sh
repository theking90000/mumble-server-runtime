#!/usr/bin/env bash
#
# gates.sh — interdictions structurelles R4 de la roadmap Mumble Server Runtime.
#
# Ces gates encodent les frontières du guide d'implémentation et les ADR encore
# actifs. Ils échouent dès qu'un agent introduit un motif interdit. Si un agent
# doit contourner un gate, c'est l'architecture qui a un problème, pas le gate
# (R4).
#
# Sortie : exit 0 si aucune violation, exit 1 sinon, avec un rapport lisible.
#
# Limites connues (grep n'est pas un compilateur) :
#   - "unwrap hors tests" est approximé en excluant les chemins contenant
#     `test` et les lignes annotées `// gate:allow-unwrap`.
#   - la détection `unsafe sans SAFETY` regarde les 3 lignes précédentes.
# Ces heuristiques sont conservatrices : elles peuvent demander une annotation
# explicite, jamais laisser passer silencieusement.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

violations=0

# Récupère les .rs sous <crate>/src, en excluant les fichiers de test.
crate_src_files() {
  local crate
  for crate in "$@"; do
    [ -d "$crate/src" ] || continue
    find "$crate/src" -name '*.rs' ! -path '*/tests/*' ! -name '*_test.rs' 2>/dev/null || true
  done
}

report() {
  echo "  ✗ $1"
  violations=$((violations + 1))
}

# grep un motif dans un ensemble de fichiers ; signale chaque hit.
# $1 = libellé, $2 = regex, $3.. = fichiers
forbid() {
  local label="$1"; shift
  local regex="$1"; shift
  [ "$#" -gt 0 ] || return 0
  local hits
  hits="$(grep -nE "$regex" "$@" 2>/dev/null | grep -v 'gate:allow' || true)"
  if [ -n "$hits" ]; then
    while IFS= read -r line; do
      report "[$label] $line"
    done <<< "$hits"
  fi
}

echo "== Gates par-crate (R4) =="

# --- mumble-server-runtime-shard : logique et publication sans sockets ---
mapfile -t shard_files < <(crate_src_files \
  mumble-server-runtime-shard runtime/crates/shard)
forbid "shard/no-net" \
       '(std::net|tokio::net|TcpListener|TcpStream|UdpSocket)' "${shard_files[@]}"

# --- Crates centrales : aucun concept du flavor de démonstration ---
central_crates=(
  "protocol|mumble-server-runtime-protocol|runtime/crates/protocol"
  "crypto|mumble-server-runtime-crypto|runtime/crates/crypto"
  "shard|mumble-server-runtime-shard|runtime/crates/shard"
  "gateway|mumble-server-runtime-gateway|runtime/crates/gateway"
)
for central_spec in "${central_crates[@]}"; do
  IFS='|' read -r central old_dir new_dir <<< "$central_spec"
  mapfile -t central_files < <(crate_src_files "$old_dir" "$new_dir")
  forbid "$central/no-demo-flavor" \
         '([Aa]urora|[Bb]orealis|mumble-server-runtime[_-]arena)' "${central_files[@]}"
done

# --- mumble-server-runtime-protocol / mumble-server-runtime-crypto : crates purs, sans runtime ni IO ---
pure_crates=(
  "protocol|mumble-server-runtime-protocol|runtime/crates/protocol"
  "crypto|mumble-server-runtime-crypto|runtime/crates/crypto"
)
for pure_spec in "${pure_crates[@]}"; do
  IFS='|' read -r pure old_dir new_dir <<< "$pure_spec"
  mapfile -t pure_files < <(crate_src_files "$old_dir" "$new_dir")
  forbid "$pure/no-tokio"      '\btokio\b'                          "${pure_files[@]}"
  forbid "$pure/no-net"        'std::net'                           "${pure_files[@]}"
  forbid "$pure/no-fs"         'std::fs'                            "${pure_files[@]}"
done

echo "== Gates globaux (tout le workspace) =="

mapfile -t all_files < <(find . -type d -name target -prune -o -name '*.rs' ! -path '*/tests/*' ! -name '*_test.rs' -print 2>/dev/null || true)
if [ "${#all_files[@]}" -gt 0 ]; then
  forbid "no-unwrap"           '\.unwrap\(\)'                       "${all_files[@]}"
  forbid "no-static-mut"       '\bstatic +mut\b'                    "${all_files[@]}"

  # unsafe sans commentaire SAFETY dans les 3 lignes précédentes.
  for f in "${all_files[@]}"; do
    awk -v file="$f" '
      /SAFETY:/ { safe = NR }
      /\bunsafe\b/ && !/gate:allow/ {
        if (NR - safe > 3 || safe == 0) {
          printf "  ✗ [no-unsafe-without-SAFETY] %s:%d: %s\n", file, NR, $0
          bad++
        }
      }
      END { exit (bad > 0) }
    ' "$f" || violations=$((violations + 1))
  done
fi

echo
if [ "$violations" -eq 0 ]; then
  echo "✓ gates.sh : aucune violation R4."
  exit 0
else
  echo "✗ gates.sh : $violations violation(s) R4."
  echo "  (Annoter une ligne avec 'gate:allow' n'est légitime qu'après revue humaine.)"
  exit 1
fi
