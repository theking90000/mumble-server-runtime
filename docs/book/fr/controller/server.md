# Référence : le serveur Rust

> Fonctionnement interne et liste des options. La page [Getting started](getting-started.md) contient les éléments nécessaires au lancement du processus.

`mumble-controller-server` intègre le protocole Controller au-dessus du gateway et du runtime de shard existants. Un unique acteur Tokio sérialise les leases de session, l'ownership des participants, les credentials de connexion, les observations, la durée de vie des Spaces et les associations de connexions Mumble. Il n'existe aucun verrou distribué ni opération de compare-and-swap au niveau applicatif.

Chaque Space matérialisé possède son propre shard dynamique. Sa `ShardLogic` lit un snapshot immuable et versionné publié par l'acteur, sans attente au niveau gRPC. L'observateur de réconciliation de la couche de composition associe la révision de snapshot consommée par `render()` à la génération de runtime correspondante. Ce mécanisme permet au protocole de distinguer les jalons (watermarks) `accepted`, `applied` et `published`.

## Démarrage du processus

Fourniture d'un certificat Mumble persistant sous forme de fichiers PEM :

```sh
cargo run -p mumble-controller-server -- \
  --mumble-cert server-cert.pem \
  --mumble-key server-key.pem
```

Utilisation recommandée uniquement pour le développement local :

```sh
cargo run -p mumble-controller-server -- --dev-self-signed
```

Le listener du Controller écoute par défaut sur `127.0.0.1:4000`. L'association à une adresse hors loopback nécessite le flag `--allow-unauthenticated-controller-network`, la version v1 fonctionnant en plaintext sans authentification des sessions Java. Le listener Mumble écoute par défaut sur `0.0.0.0:64738` en TLS/TCP et en UDP.

| Option | Valeur par défaut |
|---|---:|
| `--lease-seconds` | 30 |
| `--empty-space-grace-seconds` | 30 |
| `--max-sessions` | 64 |
| `--max-participants` | 10 000 |
| `--max-participants-per-session` | 5 000 |
| `--max-spaces` | 1 024 |
| `--max-observations-per-session` | 1 024 |
| `--queue-capacity` | 1 024 |
| `--grpc-max-frame-bytes` | 4 MiB |
| `--max-mumble-connections` | 100 |

## Cycle de vie

La perte d'un stream gRPC ne libère pas les participants. Leur ownership et leurs connexions Mumble sont maintenus jusqu'à l'expiration du lease métier de 30 secondes. L'exécution de `CloseSession`, la libération d'un participant avec son token exact ou l'expiration du lease entraîne la fermeture immédiate de la connexion Mumble correspondante.

Le premier participant placé dans un Space provoque la création de son shard. Les participants déconnectés restent présents dans le `SpaceSnapshot` en lecture seule, sans être rendus comme utilisateurs Mumble. Lorsque le dernier participant quitte le Space, le shard est conservé durant une période de grâce de 30 secondes. Toute matérialisation ultérieure de la même clé se voit attribuer un nouvel identifiant d'incarnation opaque.

Validation de l'interopérabilité via le script :

```sh
ci/controller-interop.sh
```

Ce script démarre le serveur Rust, le pilote à l'aide d'une `ControllerSession` Java 8, et connecte des clients Mumble simulés stricts en TLS et UDP. Les credentials de connexion transitent par un fichier temporaire privé et ne sont jamais journalisés.
