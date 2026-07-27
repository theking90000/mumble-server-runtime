#!/usr/bin/env bash
#
# bench-publication.sh — coût d'une publication complète (P7 T8).
#
# Mesure le chemin froid d'un changement de snapshot : rendu de toutes les
# connexions, validation des sorties, planification des transitions, commit et
# republication de la table de routage. La spec §27.4 interdit d'optimiser ce
# chemin avant de l'avoir mesuré ; ce script est cette mesure.
#
# Il n'échoue pas sur un seuil de temps : une machine de CI n'est pas un banc
# stable. Il échoue si la publication elle-même échoue, et imprime le tableau
# pour que la régression soit visible dans le journal.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== Publication complète (rendu + validation + plan + commit) =="
cargo run --release --quiet -p bench-publication
