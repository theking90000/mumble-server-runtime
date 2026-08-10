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

## Implémentation Controller

`control-plane` contient le contrat Protobuf/gRPC v1, le SDK Java 8
`ControllerSession`, le serveur Rust composé au-dessus du runtime et la
référence Bukkit Spaces. Le serveur possède les sessions, leases, fencing,
révisions et publications de Spaces. L'interop de CI relie une vraie session
Java au serveur Rust puis à des clients Mumble simulés.

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
(cd control-plane && ./gradlew check)
```

Les tests live ouvrent des sockets loopback locales.
