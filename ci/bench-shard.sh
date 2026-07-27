#!/usr/bin/env bash
#
# bench-shard.sh — coût d'un tour de shard, en regard d'une publication complète.
#
# Les deux binaires mesurent le MÊME changement métier (un membre change de
# realm) sur les MÊMES tailles, avec la même statistique. Les lire côte à côte
# est tout l'intérêt : le modèle par connexion matérialise N vues de taille O(N),
# le modèle à shards rend une fois et filtre un delta.
#
# Comme bench-publication.sh, ce script n'échoue pas sur un seuil de temps : une
# machine de CI n'est pas un banc stable. Il échoue si une publication échoue, et
# imprime les deux tableaux pour que la régression soit visible dans le journal.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== Tour de shard (rendu 1x + plan 1x + journal + routage + composition N) =="
cargo run --release --quiet -p bench-shard

echo
echo "== Publication par connexion (rendu N + validation + plan + commit) =="
cargo run --release --quiet -p bench-publication
