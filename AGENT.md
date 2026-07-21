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
| `voxloom-audio/src`          | `Mutex`, `RwLock`, `.await` (chemin par-paquet), `Box<dyn Fn>`, dépendre de `voxloom-render` / `voxloom-state` |
| `voxloom-render/src`         | importer `voxloom-protocol` (le renderer ignore le wire format) |
| `voxloom-state/src`          | types protocolaires Mumble / importer `voxloom-protocol` |
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
voxloom-audio    -/->  voxloom-state       voxloom-state    -/->  voxloom-protocol
voxloom-protocol -/->  tokio               voxloom-crypto   -/->  tokio
```

## Carte des crates (spec §7.1) — remplis-les au fil des phases, jamais avant

`voxloom-protocol` (framing/protobuf/UDP) · `voxloom-crypto` (OCB2/nonces/rejeu) ·
`voxloom-transport` (TLS/sockets) · `voxloom-session` · `voxloom-auth` ·
`voxloom-state` (état canonique/révisions) · `voxloom-render` (VDOM/rendu) ·
`voxloom-reconcile` (diff/plan) · `voxloom-audio` (routage) · `voxloom-control` ·
`voxloom-observe` · `voxloom-testkit` (client simulé/proptest/fuzz).

## Ordre des phases (résumé)

`P0 corpus/refs → P1 codec → P2 proxy oracle → P3 serveur minimal → P4 hot path`,
avec `P5 moteur de vues pur` parallélisable dès P2. Détail et critères de « done »
dans `docs/voxloom-roadmap-agents-v0_1.md`. **Phase courante : P1 (codec pur),
quasi terminée** — tâches 1 à 6 faites (framing, protobuf, UDPTunnel, enveloppe
UDP, OCB2, fuzzing) ; reste le binaire `corpus-decode` (critère de « done »).
État détaillé et reprise : **`docs/STATUS.md`**.

## Points de contrôle humains

`P0` captures corpus + décision legacy UDP · `P2` audio à travers le proxy ·
`P3` premier handshake client officiel · `P4` premier appel deux clients ·
`P6` comportement client sur vues dynamiques · `P9` compatibilité multi-clients ·
**+ toute modification de `conformance/` ou du testkit (R2).**

## Conventions du dépôt

- Toolchain pinnée : `rust-toolchain.toml` (1.93.0). Édition 2024.
- Commits : messages clairs, impératifs, **sans** ligne `Co-Authored-By`.
- Avant de committer : gates verts + `cargo fmt` + `cargo clippy` propres.
