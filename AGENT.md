# AGENT.md — guide opérationnel pour agents d'implémentation

Ce dépôt implémente **Voxloom**, un runtime vocal déclaratif compatible Mumble.
Ce fichier est le contrat de travail. La roadmap (`docs/voxloom-roadmap-agents-v0_1.md`)
et la spécification (`docs/voxloom-specification-technique-v0.1.md`) font autorité ;
en cas de conflit, la spec l'emporte, et un détail protocolaire non tranché
**s'arrête et se signale**, il ne s'invente pas.

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

| Zone                         | Interdit |
|------------------------------|----------|
| `voxloom-audio/src`          | `Mutex`, `RwLock`, `.await` (chemin par-paquet), `Box<dyn Fn>`, dépendre de `voxloom-render` / `voxloom-flavor` |
| `voxloom-render/src`         | importer `voxloom-protocol` (le renderer ignore le wire format) |
| `voxloom-flavor/src`         | types protocolaires Mumble, importer `voxloom-protocol` ou définir un métier concret |
| `voxloom-protocol`, `voxloom-crypto` | `tokio`, IO (`std::net`, `std::fs`) — crates purs |
| tout le workspace            | `.unwrap()` hors tests, `static mut`, `unsafe` sans commentaire `// SAFETY:` |

Échappatoire réservée à la revue humaine : annoter une ligne avec `gate:allow`.
Ne l'utilise pas de ta propre initiative.

Lance les gates en local avant de committer :

```bash
ci/gates.sh && ci/dep-direction.sh && ci/verifier-boundary.sh
```

## Direction des dépendances

Arêtes interdites dans le graphe cargo (`ci/dep-direction.sh`) :

```
voxloom-audio    -/->  voxloom-render      voxloom-render   -/->  voxloom-protocol
voxloom-audio    -/->  voxloom-flavor      voxloom-flavor   -/->  voxloom-protocol
voxloom-protocol -/->  tokio               voxloom-crypto   -/->  tokio
```

## Carte des crates (spec §7.1) — remplis-les au fil des phases, jamais avant

`voxloom-protocol` (framing/protobuf/UDP) · `voxloom-crypto` (OCB2/nonces/rejeu) ·
`voxloom-transport` (TLS/sockets) · `voxloom-session` · `voxloom-auth` ·
`voxloom-flavor` (contrat de snapshot/rendu métier opaque) · `voxloom-render` (VDOM/rendu) ·
`voxloom-reconcile` (diff/plan) · `voxloom-audio` (routage) · `voxloom-control` ·
`voxloom-observe` · `voxloom-testkit` (client simulé/proptest/fuzz) ·
`voxloom-shard` (runtime à shards : portées, journal de deltas, composition par
connexion — étapes 1 à 7 de `docs/design/guide-implementation.md`) ·
`voxloom-gateway` (la porte d'entrée du même runtime : plan de contrôle TLS,
routeur de connexions, registre multi-shards, migration, plan vocal UDP —
étapes 8 à 10). Les deux coexistent avec le pipeline P5–P7 qu'ils visent à
remplacer ; `voxloom-server` est encore sur l'ancien.

Le métier concret appartient aux flavors compilés avec l'application, jamais au
runtime Voxloom. La décision `docs/decisions/0002-flavor-owns-business-state.md`
fait autorité sur cette frontière.

## Ordre des phases (résumé)

`P0 corpus/refs → P1 codec → P2 proxy oracle → P3 serveur minimal → P4 hot path`,
avec `P5 moteur de vues pur` parallélisable dès P2. Détail et critères de « done »
dans `docs/voxloom-roadmap-agents-v0_1.md`. **P0 à P4 sont closes** (checkpoints
humains signés pour P0, P2, P3 et P4) ; le cœur pur de P5 est fait et P6 est
close, **et P7 aussi** (checkpoint humain signé le 2026-07-27 : le flavor de
référence tourne en live à travers la seule API publique).
**Phase courante : P8 (flavor Minecraft).**
État détaillé et reprise :
**`docs/STATUS.md`**, qui fait foi sur l'avancement.

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
