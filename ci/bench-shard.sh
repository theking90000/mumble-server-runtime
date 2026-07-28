#!/usr/bin/env bash
#
# bench-shard.sh — coût d'un tour de shard.
#
# Ce script n'échoue pas sur un seuil de temps : une machine de CI n'est pas un
# banc stable. Il échoue si un tour échoue et imprime le tableau pour rendre les
# évolutions visibles dans le journal.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== Tour de shard (rendu 1x + plan 1x + journal + routage + composition N) =="
cargo run --release --quiet -p bench-shard
