# État de Mumble Server Runtime

## Runtime courant

Les dix étapes de `docs/design/guide-implementation.md` sont implémentées. Le
chemin de production est :

```text
application / flavor
        |
        v
mumble-server-runtime-gateway -> mumble-server-runtime-shard -> mumble-server-runtime-protocol
        |
        +-----------------------> mumble-server-runtime-crypto
```

- `mumble-server-runtime-shard` possède les portées, le constructeur de rendu, la vue
  partagée, les overlays privés, le diff/plan, le journal, les files bornées et
  la table de routage audio ;
- `mumble-server-runtime-gateway` possède TLS/TCP/UDP, le handshake Mumble, le registre de
  connexions, les shards et les migrations ;
- `runtime/reference/arena` est le flavor de démonstration et le binaire de
  composition ;
- `runtime/tools/stress` exerce le gateway avec des clients Mumble headless ;
- `mumble-server-runtime-testkit` est le verificateur indépendant : modèle client strict,
  scénarios gateway live et oracle de publications shard.

Le benchmark actif est `ci/bench-shard.sh`.

## Fondation d'intégration Controller

`integrations/controller` contient le contrat Protobuf/gRPC v1 et le SDK Java 8
`ControllerSession`. Le SDK maintient un état désiré, réconcilie ses handles de
participants et cache des projections de Spaces à travers une frontière de
transport testable.

Cette fondation n'est pas reliée au runtime courant : aucun `adapter-rust`,
serveur gRPC de production ou flavor Minecraft n'est implémenté. Elle ne prouve
donc aucune intégration live avec les crates Rust.

## Pipeline retiré

Le pipeline exploratoire par connexion P4–P7 a été supprimé du workspace :

- `mumble-server-runtime-render`
- `mumble-server-runtime-reconcile`
- `mumble-server-runtime-audio`
- `mumble-server-runtime-session`
- `mumble-server-runtime-flavor`
- `mumble-server-runtime-control`
- `mumble-server-runtime-flavor-reference`
- `mumble-server-runtime-server`
- `tools/mumble-server-runtime-aurora`
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
(cd integrations/controller && ./gradlew check)
```

Les tests live ouvrent des sockets loopback locales.
