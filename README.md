# Voxloom

Runtime vocal déclaratif compatible Mumble : un serveur qui parle le protocole
Mumble au fil, mais dont l'état canonique, les vues par connexion et le routage
audio sont indépendants du protocole. Voir `docs/` pour la spécification et la
roadmap.

**Statut : Phase 0 — infrastructure de vérité.** Le workspace est volontairement
vide (aucun crate de domaine) tant que les oracles ne sont pas en place. Voir
`AGENT.md`.

## Structure

```
AGENT.md                     contrat de travail (règles R1–R6, gates, phases)
Cargo.toml                   workspace virtuel (membres ajoutés au fil des phases)
ci/                          gates R2/R4 exécutables en local et en CI
docs/                        spécification, roadmap, décisions (ADR/decisions)
references/                  sources protocolaires vendored (Mumble, pinné)
fixtures/corpus/             captures binaires réelles annotées (zone vérificateur)
conformance/                 tests de conformité (zone vérificateur, R2)
```

## Développement

```bash
ci/gates.sh                  # interdictions structurelles R4
ci/dep-direction.sh          # direction des dépendances entre crates
ci/verifier-boundary.sh      # séparation implémenteur/vérificateur R2
ci/cargo-gate.sh cargo test --workspace   # tests (no-op si workspace vide)
```

Toolchain pinnée dans `rust-toolchain.toml` (Rust 1.93, édition 2024).
