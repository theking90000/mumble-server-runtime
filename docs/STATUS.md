# État du projet & reprise (handoff)

> Document vivant. Il décrit ce qui est fait, ce qui reste, et ce qu'un agent qui
> reprend doit savoir. Autorité : la spec et la roadmap (`docs/`) ; ce fichier ne
> fait que pointer l'état courant. Mettre à jour à chaque fin de tâche.

**Phase courante : P1 (codec pur) — tâches 1 à 6 terminées.** Reste le binaire
`corpus-decode` (critère de « done » de P1). Les règles R1–R6 (`AGENT.md`) et la
discipline de code restent la loi.

---

## Fait

### Phase 0 — infrastructure de vérité (close)

- Corpus : 7 scénarios réels sous `fixtures/corpus/` (`01-handshake` →
  `07-disconnect`). `08-crypto-resync` et la session legacy Mumla ne sont **pas**
  capturés (non provocables / legacy tranché *out* en ADR-0001) ; corpus accepté
  comme clos à 7 par le point de contrôle humain.
  - Réserve : `05-whisper` et `06-permission-denied` peuvent ne pas exercer le
    comportement voulu — sans effet sur P1 (le décodeur lit tous les octets),
    à revérifier pour les golden tests de P3.
- Références vendorées (R1) : mumble-voip **v1.5.915**, commit
  `5fe5ec6e61b0c1cc414a8a8db548ec484eec6b90` (`references/mumble.pin`). Extraits
  sous `references/vendored/` (`Mumble.proto`, `MumbleUDP.proto`,
  `ocb2-vectors/`, `protocol/MumbleProtocol.h` + `Connection.cpp`). Provenance :
  `references/vendored/PROVENANCE.md`. Le clone brut n'est pas commité.

### Phase 1 — codec pur (tâches 1–6)

Deux crates purs (sans IO ni runtime, `#![forbid(unsafe_code)]`, `[lints]
workspace = true`) :

| Tâche | Livrable | Fichier |
|------|----------|---------|
| 1 | Framing TCP incrémental (`parse_frame`/`write_frame`, en-tête 6 o, limite `0x7fffff`, registre `TcpMessageType`) | `voxloom-protocol/src/framing.rs` |
| 2 | Messages protobuf générés (prost + protox, build hermétique sans `protoc`) | `voxloom-protocol/src/messages.rs`, `build.rs` |
| 3 | Décodage typé `ControlMessage` ; cas spécial **UDPTunnel = octets bruts** + test de non-régression | `voxloom-protocol/src/control.rs` |
| 4 | Enveloppe UDP protobuf (`[type:u8][protobuf]`, Audio=0/Ping=1, legacy refusé, Opus non décodé) | `voxloom-protocol/src/udp.rs` |
| 5 | OCB2-AES128 porté depuis la référence, validé contre les vecteurs ; `CryptState` (IV recovery, anti-rejeu) | `voxloom-crypto/src/ocb2.rs` |
| 6 | Cibles cargo-fuzz (framing/control/udp/ocb2_decrypt) + smoke tests stables | `fuzz/`, `ci/fuzz-smoke.sh`, `*/tests/fuzz_smoke.rs` |

Chaque fait protocolaire porte un `// REF:` vers `references/vendored/`.

**Vérifié** (tout sous `RUSTFLAGS="-D warnings"`) : `ci/gates.sh`,
`ci/dep-direction.sh`, `cargo fmt --check`, `cargo clippy --workspace
--all-targets`, `cargo test --workspace` — tous verts. cargo-fuzz local : aucun
crash (framing 20.9M, control 4.4M, udp 3.0M, ocb2_decrypt 847k exécutions).

Commits (sur `main`, sans `Co-Authored-By`) : `5fef92f` refs, `153a875` corpus,
`19d8f92` framing, `2c32552` protobuf, `5c8c538` control, `9a5d953` udp,
`0f53017` OCB2, `9846536` fuzzing.

---

## Reste à faire

### 1. Binaire `corpus-decode` — critère de « done » de P1 (prioritaire)

**But :** relire chaque capture `fixtures/corpus/*/session.voxcap` et produire un
transcript lisible complet, sans octet inexpliqué. C'est le smoke test ultime du
codec contre le trafic réel.

Ce qu'il doit faire :

1. **Lire le `.voxcap`.** Le format (`VOXCAP01`, puis enregistrements
   `[dir:u8][transport:u8][ts:i64 LE][len:u32 LE][data]`) et un lecteur
   (`read_records`) existent déjà dans `tools/recording-proxy/src/capture.rs`,
   mais **ne sont pas partagés**. Décider : extraire le lecteur `.voxcap` dans un
   endroit réutilisable, ou le ré-implémenter dans le binaire. Ne pas rendre
   `voxloom-protocol`/`voxloom-crypto` dépendants de l'IO (gate R4) : le binaire
   vit sous `tools/` ou dans son propre crate, pas dans les crates purs.
2. **TCP :** par direction (C2S et S2C sont deux flux distincts), concaténer les
   `data` des enregistrements TCP, dérouler `parse_frame` en incrémental (les
   enregistrements sont des morceaux de flux, pas des messages alignés), puis
   `decode_control` chaque frame et imprimer type + champs.
3. **UDP :** les datagrammes du corpus sont **chiffrés OCB2** (le proxy a relayé
   l'UDP en aveugle). Pour les décoder :
   - Extraire la clé et les nonces du message TCP **`CryptSetup`** (type 15,
     `MumbleProto.CryptSetup` : `key`, `client_nonce`, `server_nonce`).
   - Construire des `CryptState` (`voxloom-crypto`) pour déchiffrer, puis
     `decode_udp` sur le clair.
   - **Piège R1 :** le mapping direction → (encrypt_iv/decrypt_iv) et quel nonce
     sert à quel sens **doit être vérifié dans la source vendorée**
     (`CryptSetup` handling côté client/serveur), pas deviné. Si un détail manque,
     s'arrêter et le signaler.
4. **Sortie :** transcript lisible (horodatage, direction, transport, message
   décodé). Propriété à tenir : décodage total du corpus, aucun octet inexpliqué.

**Done attendu :** `cargo run -p <bin> -- fixtures/corpus/01-handshake` (et les 6
autres) produit un transcript complet ; ajouter un test qui décode au moins un
scénario de bout en bout sans erreur.

### 2. Job nightly cargo-fuzz en CI (différé, optionnel)

`ci/fuzz-smoke.sh` existe et tourne en local. L'intégrer comme **job nightly**
dans `.github/workflows/ci.yml` (toolchain nightly + `cargo install cargo-fuzz`
ou `taiki-e/install-action@cargo-fuzz`, budget court). Différé car non
vérifiable en local (GitHub Actions). Le smoke stable (`*/tests/fuzz_smoke.rs`)
couvre déjà le contrat « aucune panique » dans la CI stable existante.

---

## Reprise : commandes et pièges

### Vérifier l'état (à lancer avant/après toute modif)

```bash
ci/gates.sh && ci/dep-direction.sh && ci/verifier-boundary.sh
RUSTFLAGS="-D warnings" cargo fmt --all --check
RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --all-features
cargo test --workspace
ci/fuzz-smoke.sh 30        # nightly + cargo-fuzz requis, sinon skip propre
```

### Pièges connus (traps)

- **CI = `RUSTFLAGS="-D warnings"`.** Promeut le `expect_used = "warn"` du
  workspace en erreur dans tout crate qui active `[lints] workspace = true`. Les
  modules de test doivent porter `#![allow(clippy::expect_used)]` (avec
  justification). Le proxy y échappe car il n'active pas les lints workspace.
- **Code généré prost + clippy :** attention `needless_range_loop` sous
  `-D warnings` — préférer les boucles par itérateur.
- **Lecteur `.voxcap`** confiné à `tools/recording-proxy` (voir corpus-decode).
- **Frontière vérificateur (R2, `ci/verifier-boundary.sh`) :** ne jamais toucher
  `fixtures/`, `conformance/` ou `voxloom-testkit/` dans le **même diff** qu'un
  `voxloom-*/src`. Les commits ont été séparés exprès ; en mode PR, la capture de
  corpus doit être une PR distincte.
- **cargo-fuzz** exige nightly + l'outil ; le crate `fuzz/` est un workspace
  détaché (le `cargo test --workspace` racine l'ignore).
- **Commits sans `Co-Authored-By`**, réguliers, un par tranche vérifiée.
- **Crates purs** (`voxloom-protocol`, `voxloom-crypto`) : ni `tokio`, ni
  `std::net`/`std::fs` dans `src/` (gates R4). Les build scripts en sont exemptés.

### Carte des crates existants

```
tools/recording-proxy/   binaire P0 (proxy d'enregistrement + lecteur .voxcap)
voxloom-protocol/         framing, messages prost, control, udp   (pur)
voxloom-crypto/           ocb2 (OCB2-AES128, CryptState)          (pur)
fuzz/                     cibles cargo-fuzz (workspace détaché, nightly)
references/vendored/      vérité protocolaire (R1), pin v1.5.915
fixtures/corpus/          7 captures réelles (zone vérificateur R2)
```

### Après P1

`P2 proxy MITM comme oracle vivant` (déchiffre/réencode le trafic réel à travers
le proxy). `P5 moteur de vues pur` est parallélisable dès P2. Voir la roadmap.
