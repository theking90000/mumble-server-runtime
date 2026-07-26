#!/usr/bin/env bash
#
# gates.sh — interdictions structurelles R4 de la roadmap Voxloom.
#
# Ces gates encodent les ADR 001, 002 et 005. Ils sont volontairement actifs
# AVANT la première ligne de logique (Phase 0) : ils ne trouvent rien tant que
# les crates n'existent pas, et échouent dès qu'un agent introduit un motif
# interdit. Si un agent doit contourner un gate, c'est l'architecture qui a un
# problème, pas le gate (R4).
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
  local crate="$1"
  [ -d "$crate/src" ] || return 0
  find "$crate/src" -name '*.rs' ! -path '*/tests/*' ! -name '*_test.rs' 2>/dev/null || true
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

# --- voxloom-audio : hot path sans lock, sans await, sans callback, isolé ---
mapfile -t audio_files < <(crate_src_files voxloom-audio)
forbid "audio/no-Mutex"        '\b(Mutex|RwLock)\b'                "${audio_files[@]}"
forbid "audio/no-await"        '\.await\b'                          "${audio_files[@]}"
forbid "audio/no-boxed-fn"     'Box<dyn +Fn'                        "${audio_files[@]}"
forbid "audio/no-render-dep"   'voxloom[_-]render'                  "${audio_files[@]}"
forbid "audio/no-flavor-dep"   'voxloom[_-]flavor'                  "${audio_files[@]}"

# --- voxloom-render : ignore le wire format ---
mapfile -t render_files < <(crate_src_files voxloom-render)
forbid "render/no-protocol"    'voxloom[_-]protocol'                "${render_files[@]}"

# --- voxloom-flavor : contrat générique sans types protocolaires Mumble ---
mapfile -t flavor_files < <(crate_src_files voxloom-flavor)
forbid "flavor/no-protocol"    'voxloom[_-]protocol'                "${flavor_files[@]}"
forbid "flavor/no-domain"      '([Mm]inecraft|[Rr]ealm|[Pp]layer|[Tt]eam|[Pp]osition|[Rr]adio)' \
                                                                    "${flavor_files[@]}"

# --- voxloom-protocol / voxloom-crypto : crates purs, sans runtime ni IO ---
for pure in voxloom-protocol voxloom-crypto; do
  mapfile -t pure_files < <(crate_src_files "$pure")
  forbid "$pure/no-tokio"      '\btokio\b'                          "${pure_files[@]}"
  forbid "$pure/no-net"        'std::net'                           "${pure_files[@]}"
  forbid "$pure/no-fs"         'std::fs'                            "${pure_files[@]}"
done

echo "== Gates globaux (tout le workspace) =="

mapfile -t all_files < <(find . -path ./target -prune -o -name '*.rs' ! -path '*/tests/*' ! -name '*_test.rs' -print 2>/dev/null || true)
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
