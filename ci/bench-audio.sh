#!/usr/bin/env bash
#
# bench-audio.sh — coût par paquet et par destinataire du routeur (P4, T6).
#
# Le bench criterion mesure ; ce script en fait un test de non-régression. Deux
# garde-fous, volontairement de natures différentes :
#
#   1. Un PLAFOND ABSOLU par destinataire, comparé au coût du relais complet.
#      Il est fixé avec un ordre de grandeur de marge, donc il survit à une
#      machine de CI lente, mais pas à une régression structurelle : un verrou
#      pris dans le chemin par paquet, une recompilation du snapshot par
#      datagramme ou un décodage Opus le font exploser d'un facteur 100.
#      C'est ce plafond qui rend le gate portable d'une machine à l'autre.
#
#   2. Une COMPARAISON À BASELINE criterion, quand une baseline locale existe
#      (`ci/bench-audio.sh save` la crée). Bien plus fine, mais valable seulement
#      sur la même machine : criterion range ses baselines sous target/, qui
#      n'est pas versionné.
#
# Usage :
#   ci/bench-audio.sh            # mesure + plafond absolu (+ baseline si dispo)
#   ci/bench-audio.sh save       # enregistre la baseline locale « p4 »
#
# La mesure de référence prise le 2026-07-26 (Apple Silicon, profil bench) est
# consignée dans docs/STATUS.md.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Plafond du coût par destinataire, en nanosecondes, sur le relais complet
# (consultation du snapshot + décision + construction de l'enveloppe). Mesuré à
# ~21 ns ; 500 ns laisse plus d'un ordre de grandeur de marge.
CEILING_NS_PER_RECIPIENT=500

# Le point de mesure : le plus grand nombre de destinataires du bench, là où le
# coût par destinataire est le plus représentatif.
CASE="relay/128"
CASE_RECIPIENTS=128

MODE="${1:-check}"

# Seuil de bruit à 20 %. Les cas les plus rapides se mesurent en dizaines de
# nanosecondes, où l'écart run-à-run dépasse couramment le seuil criterion par
# défaut (2 %) : gardé tel quel, le gate échouerait au hasard, donc ne vaudrait
# rien. À 20 % il reste largement sensible — une régression structurelle se
# compte en facteurs, pas en pourcents — tout en étant reproductible.
criterion_args=(--warm-up-time 1 --measurement-time 3 --noise-threshold 0.2)
compared_to_baseline=0
case "$MODE" in
  save)
    criterion_args+=(--save-baseline p4)
    ;;
  check)
    if [ -d "target/criterion/relay/128/p4" ]; then
      criterion_args+=(--baseline p4)
      compared_to_baseline=1
    else
      echo "bench-audio.sh : pas de baseline locale « p4 » — seul le plafond absolu s'applique."
      echo "                 (ci/bench-audio.sh save pour en enregistrer une)"
    fi
    ;;
  *)
    echo "bench-audio.sh : mode inconnu « $MODE » (attendu : check | save)" >&2
    exit 2
    ;;
esac

output="$(cargo bench -p voxloom-audio --bench routing -- "${criterion_args[@]}" 2>&1)"
echo "$output"

if [ "$MODE" = "save" ]; then
  echo
  echo "✓ bench-audio.sh : baseline « p4 » enregistrée."
  exit 0
fi

# --- Garde-fou 1 : plafond absolu -------------------------------------------
# Une ligne criterion ressemble à :
#   relay/128               time:   [2.7321 µs 2.7378 µs 2.7441 µs]
# On prend l'estimation centrale et on la normalise en nanosecondes.
measured_ns="$(printf '%s\n' "$output" | awk -v case_name="$CASE" '
  # $3..$8 = [low_value low_unit mid_value mid_unit high_value high_unit]
  $1 == case_name && $2 == "time:" {
    value = $5; unit = $6
    if (unit == "ps")      factor = 0.001
    else if (unit == "ns") factor = 1
    else if (unit == "µs" || unit == "us") factor = 1000
    else if (unit == "ms") factor = 1000000
    else if (unit == "s")  factor = 1000000000
    else next
    printf "%.3f", value * factor
    exit
  }
')"

if [ -z "$measured_ns" ]; then
  echo "✗ bench-audio.sh : impossible de lire le temps de $CASE dans la sortie criterion." >&2
  exit 1
fi

per_recipient="$(awk -v total="$measured_ns" -v n="$CASE_RECIPIENTS" 'BEGIN { printf "%.2f", total / n }')"
echo
echo "== coût par destinataire ($CASE) : ${per_recipient} ns (plafond ${CEILING_NS_PER_RECIPIENT} ns) =="

if awk -v measured="$per_recipient" -v ceiling="$CEILING_NS_PER_RECIPIENT" \
   'BEGIN { exit !(measured > ceiling) }'; then
  echo "✗ bench-audio.sh : ${per_recipient} ns/destinataire dépasse le plafond de ${CEILING_NS_PER_RECIPIENT} ns." >&2
  echo "  Une régression de cet ordre est structurelle (verrou dans le chemin par paquet," >&2
  echo "  recompilation du snapshot par datagramme, décodage Opus), pas du bruit de mesure." >&2
  exit 1
fi

# --- Garde-fou 2 : régression relevée par criterion --------------------------
# Uniquement contre la baseline explicite. Sans elle, criterion compare au run
# précédent, dont l'écart n'est que du bruit de machine : croire ce signal-là
# ferait échouer le gate au hasard, ce qui revient à ne plus avoir de gate.
if [ "$compared_to_baseline" -eq 1 ] && printf '%s\n' "$output" | grep -q "Performance has regressed"; then
  echo "✗ bench-audio.sh : criterion signale une régression contre la baseline « p4 »." >&2
  exit 1
fi

echo "✓ bench-audio.sh : aucune régression."
