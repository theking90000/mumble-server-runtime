# Voxloom

Runtime vocal déclaratif compatible Mumble : un serveur qui parle le protocole
Mumble au fil, mais dont l'état canonique, les vues par connexion et le routage
audio sont indépendants du protocole. Voir `docs/` pour la spécification et la
roadmap.

**Statut : phases P0 à P4 closes** (infrastructure de vérité, codec pur, proxy
oracle, serveur minimal, routage audio) ; deux vrais clients Mumble se
connectent, se voient et s'entendent, en UDP comme en repli tunnel TCP. Le cœur
pur de P5 (moteur de vues) est fait. Prochaine phase : P6. L'avancement détaillé
fait foi dans `docs/STATUS.md` ; les règles de travail sont dans `AGENT.md`.

## Structure

```
AGENT.md                     contrat de travail (règles R1–R6, gates, phases)
Cargo.toml                   workspace virtuel (membres ajoutés au fil des phases)
ci/                          gates R2/R4 exécutables en local et en CI
docs/                        spécification, roadmap, décisions (ADR), STATUS
references/                  sources protocolaires vendored (Mumble, pinné)
fixtures/corpus/             captures binaires réelles annotées (zone vérificateur)
conformance/                 tests de conformité (zone vérificateur, R2)
fuzz/                        cibles cargo-fuzz (workspace détaché, nightly)
tools/                       binaires d'outillage (proxys, décodeur de corpus)
voxloom-protocol/            framing, protobuf, enveloppe UDP        (pur)
voxloom-crypto/              OCB2-AES128, CryptState                 (pur)
voxloom-render/              vue normalisée, normalize, validate     (pur)
voxloom-reconcile/           diff, planificateur, ViewIdMapping      (pur)
voxloom-audio/               routage audio : compile, may_receive    (pur)
voxloom-server/              serveur minimal + routage voix
voxloom-testkit/             client simulé et juge des invariants (R2)
```

## Développement

```bash
ci/gates.sh                  # interdictions structurelles R4
ci/dep-direction.sh          # direction des dépendances entre crates
ci/verifier-boundary.sh      # séparation implémenteur/vérificateur R2
ci/cargo-gate.sh cargo test --workspace   # tests
ci/bench-audio.sh            # coût par destinataire du routeur (P4)
```

Toolchain pinnée dans `rust-toolchain.toml` (Rust 1.93, édition 2024).
