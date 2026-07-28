# Voxloom

Runtime vocal déclaratif compatible Mumble. Une application décrit un monde
métier sous forme de portées, vues et relations audio ; Voxloom le publie à des
clients Mumble sans posséder cet état métier.

Le runtime courant est organisé autour de deux crates :

- `voxloom-shard` rend et réconcilie une vue partagée, compose les vues privées
  et publie une table de routage sans effectuer d'IO ;
- `voxloom-gateway` gère TLS, TCP, UDP, le handshake, les connexions et les
  migrations entre shards.

`tools/voxloom-arena` fournit un flavor de démonstration exécutable.
`voxloom-testkit` reste le juge indépendant : son client simulé applique le
protocole et refuse toute violation de son modèle strict.

La référence d'architecture est
[`docs/design/guide-implementation.md`](docs/design/guide-implementation.md).
L'état détaillé est dans [`docs/STATUS.md`](docs/STATUS.md) et les règles de
travail dans [`AGENT.md`](AGENT.md). L'ancien pipeline exploratoire P4–P7 reste
consultable au tag `legacy-p7-final`.

## Développement

```bash
ci/gates.sh
ci/dep-direction.sh
ci/verifier-boundary.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features
cargo test --workspace --all-features
ci/bench-shard.sh
```

La toolchain est fixée dans `rust-toolchain.toml` (Rust 1.93, édition 2024).
