# État de Voxloom

## Runtime courant

Les dix étapes de `docs/design/guide-implementation.md` sont implémentées. Le
chemin de production est :

```text
application / flavor
        |
        v
voxloom-gateway -> voxloom-shard -> voxloom-protocol
        |
        +-----------------------> voxloom-crypto
```

- `voxloom-shard` possède les portées, le constructeur de rendu, la vue
  partagée, les overlays privés, le diff/plan, le journal, les files bornées et
  la table de routage audio ;
- `voxloom-gateway` possède TLS/TCP/UDP, le handshake Mumble, le registre de
  connexions, les shards et les migrations ;
- `tools/voxloom-arena` est le flavor de démonstration et le binaire de
  composition ;
- `tools/voxloom-stress` exerce le gateway avec des clients Mumble headless ;
- `voxloom-testkit` est le verificateur indépendant : modèle client strict,
  scénarios gateway live et oracle de publications shard.

Le benchmark actif est `ci/bench-shard.sh`.

## Pipeline retiré

Le pipeline exploratoire par connexion P4–P7 a été supprimé du workspace :

- `voxloom-render`
- `voxloom-reconcile`
- `voxloom-audio`
- `voxloom-session`
- `voxloom-flavor`
- `voxloom-control`
- `voxloom-flavor-reference`
- `voxloom-server`
- `tools/voxloom-aurora`
- `tools/bench-publication`

Son dernier état complet reste consultable au tag `legacy-p7-final`. Les
checklists signées sous `docs/checklists/`, la roadmap P0–P7 et les décisions
restent dans le dépôt comme preuves historiques ; elles ne décrivent plus la
topologie active.

## Validation

Avant publication :

```bash
ci/gates.sh
ci/dep-direction.sh
ci/verifier-boundary.sh
cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
RUSTFLAGS="-D warnings" cargo test --workspace --all-features
```

Les tests live ouvrent des sockets loopback locales.
