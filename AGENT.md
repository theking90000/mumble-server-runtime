# AGENT.md — guide opérationnel pour agents d'implémentation

Ce dépôt implémente **Mumble Server Runtime**, un runtime vocal déclaratif compatible Mumble.
Ce fichier est le contrat de travail. Le guide d'implémentation
(`docs/design/guide-implementation.md`) fait autorité sur l'architecture courante ;
la spécification (`docs/voxloom-specification-technique-v0.1.md`) reste l'autorité
protocolaire. Un détail protocolaire non tranché **s'arrête et se signale**, il ne
s'invente pas.

## Principe directeur

Un agent ne peut pas juger la conformité protocolaire. Seul un oracle le peut.
Tout est ordonné pour construire les oracles (corpus, proxy MITM, client simulé)
**avant** le code qu'ils jugent. N'inverse jamais cet ordre.

## Les six règles (R1–R6)

**R1 — La vérité protocolaire ne vient jamais de mémoire.**
Toute affirmation sur le wire format doit être traçable vers une source vendored :
`references/mumble/` (clone pinné), `fixtures/corpus/` (captures réelles), ou la
spec. Chaque module protocolaire porte un commentaire `// REF:` pointant vers le
fichier source Mumble correspondant. Un détail absent de ces sources → tu t'arrêtes
et tu le signales. Tu n'inventes pas un détail « plausible ».

**R2 — Séparation implémenteur / vérificateur.**
`voxloom-testkit/`, `fixtures/`, `conformance/` ne sont modifiables que par des
tâches de **vérification**, jamais par une tâche d'implémentation. Si tes tests
échouent, tu corriges l'implémentation, pas le test. Toute modification d'un test
de conformité passe par une **revue humaine**. Enforcement : `ci/verifier-boundary.sh`
refuse un diff qui touche à la fois `voxloom-*/src` et une zone vérificateur.

**R3 — Critère de « done » machine-vérifiable.**
Chaque tâche se termine par une commande exacte qui doit passer (`cargo test -p …`,
`cargo fuzz run … -- -max_total_time=…`, script de scénario). « Ça compile » et
« ça a l'air correct » ne sont pas des critères. Une tâche sans commande de done
est mal spécifiée : refuse-la.

**R4 — Interdictions structurelles en CI dès le jour 1.**
Gates actifs (`ci/gates.sh` + `ci/dep-direction.sh`), voir la section « Gates »
plus bas. Si un agent doit contourner un gate, c'est l'architecture qui a un
problème, pas le gate.

**R5 — Tranches verticales, binaire démontrable.**
Chaque phase se termine par un binaire ou un script qu'un humain peut lancer. Pas
de phase « que des types ». Les abstractions non exigées par la phase courante sont
interdites. **Un crate n'existe que quand une phase le remplit** : n'ajoute pas de
crate vide « en avance ».

**R6 — Fail closed par défaut.**
Tout chemin non implémenté répond par un refus explicite (`PermissionDenied`,
`Reject`, drop journalisé), jamais par un succès silencieux. Un `todo!()`
atteignable par un client est un bug de sévérité maximale (les lints workspace
refusent `todo!`/`unimplemented!`).

## Gates (R4) — ce que la CI refuse

Appliqués par `ci/gates.sh` (grep, car clippy ne peut pas exprimer ces règles
par-crate) et `ci/dep-direction.sh` (via `cargo metadata`) :

| Zone                                 | Interdit                                                                     |
| ------------------------------------ | ---------------------------------------------------------------------------- |
| `mumble-server-runtime-shard/src`                  | sockets (`std::net`, `tokio::net`, `TcpListener`, `TcpStream`, `UdpSocket`)  |
| crates centraux                      | importer le flavor de démonstration `voxloom-arena`                          |
| `mumble-server-runtime-protocol`, `mumble-server-runtime-crypto` | `tokio`, IO (`std::net`, `std::fs`) — crates purs                            |
| tout le workspace                    | `.unwrap()` hors tests, `static mut`, `unsafe` sans commentaire `// SAFETY:` |

Échappatoire réservée à la revue humaine : annoter une ligne avec `gate:allow`.
Ne l'utilise pas de ta propre initiative.

Lance les gates en local avant de committer :

```bash
ci/gates.sh && ci/dep-direction.sh && ci/verifier-boundary.sh
```

## Direction des dépendances

Arêtes interdites dans le graphe cargo (`ci/dep-direction.sh`) :

```
mumble-server-runtime-protocol -/-> tokio                mumble-server-runtime-crypto -/-> tokio
mumble-server-runtime-shard    -/-> mumble-server-runtime-gateway      mumble-server-runtime-shard  -/-> mumble-server-runtime-crypto
crates centraux  -/-> voxloom-arena        crates centraux -/-> voxloom-testkit
```

## Carte des crates courants

`mumble-server-runtime-protocol` (framing/protobuf/UDP) · `mumble-server-runtime-crypto`
(OCB2/nonces/rejeu) · `mumble-server-runtime-shard` (portées, rendu, diff/plan, journal,
composition, routage, file bornée) · `mumble-server-runtime-gateway` (TLS/TCP/UDP, handshake,
registre multi-shards, migration) · `voxloom-testkit` (client simulé et modèle
strict indépendant).

`tools/voxloom-arena` est le flavor de démonstration et le binaire de composition.
Les anciens crates par connexion P4–P7 ont été retirés ; leur dernier état reste
consultable au tag `legacy-p7-final`.

Le métier concret appartient aux flavors compilés avec l'application, jamais au
runtime Mumble Server Runtime. La décision `docs/decisions/0002-flavor-owns-business-state.md`
fait autorité sur cette frontière.

## État

Les étapes 1 à 10 du guide d'implémentation sont le runtime courant. Les phases
P0–P7 et leurs checklists décrivent l'historique exploratoire ; elles restent
consultables comme preuves, pas comme carte de code active. État détaillé :
**`docs/STATUS.md`**.

## Points de contrôle humains

`P0` captures corpus + décision legacy UDP · `P2` audio à travers le proxy ·
`P3` premier handshake client officiel · `P4` premier appel deux clients ·
`P6` comportement client sur vues dynamiques · `P7` flavor de référence en live ·
`P9` compatibilité multi-clients ·
**+ toute modification de `conformance/` ou du testkit (R2).**

## Conventions du dépôt

- Toolchain pinnée : `rust-toolchain.toml` (1.93.0). Édition 2024.
- Commits : messages clairs, impératifs, **sans** ligne `Co-Authored-By`.
- Avant de committer : gates verts + `cargo fmt` + `cargo clippy` propres.
