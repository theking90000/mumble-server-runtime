# État du projet & reprise (handoff)

> Document vivant. Il décrit ce qui est fait, ce qui reste, et ce qu'un agent qui
> reprend doit savoir. Autorité : la spec et la roadmap (`docs/`) ; ce fichier ne
> fait que pointer l'état courant. Mettre à jour à chaque fin de tâche.

**Phase courante : P4 (routage audio deux clients) — T1 à T5 faits et verts en
CI ; restent le bench criterion (T6) et le point de contrôle humain. P3 est
close : checklist humaine signée le 2026-07-26.** Les trois critères de P3 sont
tenus : client simulé rejouant le handshake sans violation §20 (CI), deux clients
simulés simultanés à vues indépendantes (CI), et vrai client officiel connecté
avec loopback serveur audible en UDP **et** en repli tunnel TCP, deux clients
réels qui se voient (humain, `docs/checklists/p3-minimal-server.md`). P2 est close
(checklist signée `2fb1e98`). P1 (codec pur) est close. Reste optionnel hérité de
P1 : le job nightly cargo-fuzz en CI. Les règles R1–R6 (`AGENT.md`) et la
discipline de code restent la loi.

**P5 (moteur de vues pur) — cœur pur implémenté et vérifié en CI, mergé sur
`main` le 2026-07-24.** `voxloom-render` (vue normalisée, normalize, validate) et
`voxloom-reconcile` (diff, planificateur, résolution clé→ID) sont faits ; il reste
le proptest autoritaire à grand volume, qui dépend du client simulé de P3 (voir
« Reste à faire »). Aucune dépendance réseau : n'attendait pas P2.

> **Piège client macOS (corrigé le 2026-07-24, commit `2fb1e98`) :** le client
> Mumble macOS (Qt/OpenSSL) segfault à la fin du handshake TLS si le proxy
> négocie **TLS 1.3** — le crash est dans l'introspection post-handshake de Qt,
> *avant* tout message Mumble, donc sans rapport avec le codec/crypto/UDP. rustls
> préférait 1.3 ; le proxy est désormais épinglé sur **TLS 1.2** (comme Murmur)
> dans `tools/mitm-proxy/src/tls.rs`. Se connecter au proxy via `127.0.0.1:64738`
> (pas `localhost`, qui résout d'abord en IPv6 que le proxy n'écoute pas).

> **Nature du corpus (corrigé le 2026-07-22, vérifié sur les octets décodés) :**
> le corpus est **mixte**, pas uniformément 1.3.4 comme l'affirmait une version
> antérieure de ce doc. Les scénarios **01–02** tapent un serveur public tiers en
> **Mumble 1.3.4** (`82.23.190.165`, `release: "1.3.4-4"`) : plan voix en **format
> legacy**, que l'ADR-0001 exclut *volontairement* de `voxloom-protocol` ;
> `decode_udp` (protobuf only) rejette — correctement — ces charges. Les scénarios
> **03–07** tapent un serveur **Mumble 1.5.857** (`192.168.129.87`, `release:
> "1.5.857"`, NixOS arm64) : plan voix en **format UDP protobuf** (introduit en
> 1.5.0, `PROTOBUF_INTRODUCTION_VERSION`).
>
> Conséquence : `decode_udp` **est déjà validé sur du trafic réel ≥ 1.5**. Les
> scénarios 03–07 décodent de 190 à 905 paquets `UDP(protobuf)` (Audio + Ping) par
> scénario, OCB2 déchiffrés, **0 rejet**, re-key du scénario 03 inclus. La « capture
> contre un serveur ≥ 1.5.0 » n'est donc **plus un reste à faire** : elle vit dans
> le corpus depuis le début. L'ADR-0001 tient (le legacy 01–02 reste hors scope) et
> n'est pas rediscuté ici.

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

### Phase 1 — `corpus-decode` (critère de « done »)

Crate `tools/corpus-decode` (binaire `corpus-decode` + lib testable). Rejoue
chaque `.voxcap` à travers `voxloom-protocol` + `voxloom-crypto` et produit un
transcript horodaté ; propriété tenue : **aucun octet inexpliqué** sur les 7
scénarios (0 octet TCP résiduel, 0 paquet UDP rejeté).

- **TCP** : par direction, réassemblage incrémental du flux (les enregistrements
  sont des morceaux, pas des messages alignés), `parse_frame` + `decode_control`.
  Tous les messages de contrôle des 7 captures se décodent.
- **UDP** : trois classes réelles, toutes vérifiées en source vendorée (R1) :
  1. **Pings de connectivité non chiffrés** (pré-crypt) : legacy 12 o (requête,
     4 octets zéro + timestamp) / 24 o (réponse : version, timestamp, compteurs),
     et ping **protobuf** (header `0x01`). Un client 1.5 émet les deux formes.
  2. **UDP chiffré OCB2** : clé + nonces extraits du `CryptSetup`. Mapping
     direction→IV vérifié dans `mumble/Messages.cpp` (`setKey(key, client_nonce,
     server_nonce)` ⇒ C2S déchiffré avec `client_nonce`, S2C avec `server_nonce`)
     et symétrie serveur dans `murmur/Messages.cpp`. **Re-key géré** : un
     `CryptSetup` complet en cours de session reconstruit l'état (le scénario 03
     en contient un ; sans ce traitement, 347/383 paquets échouaient).
  3. Charge déchiffrée, deux formats selon le serveur :
     - **protobuf** (03–07, serveur 1.5.857) : décodée structurellement par
       `decode_udp` en `UdpMessage::Audio`/`Ping`. C'est la validation réelle du
       chemin protobuf.
     - **legacy** (01–02, serveur 1.3.4) : signalée comme telle (type via
       `(header>>5)&0x7`), non décodée structurellement (ADR-0001).
- Un rejet OCB2 (rejeu / retard hors fenêtre / tag) est une issue *comptée et
  expliquée* (le vrai Murmur les jette pareil), pas une panique — 0 sur le corpus.
- Test d'intégration `tests/decode_corpus.rs` : décode les 7 scénarios, exige 0
  octet résiduel et 0 rejet ; verrouille la régression du re-key (scénario 03) et
  la **nature mixte du corpus** — 03–07 produisent du `UDP(protobuf) Audio` réel
  et 0 charge legacy, 01–02 l'inverse (garde contre le retour du mythe « tout 1.3.4 »).
- Lecteur `.voxcap` **ré-implémenté** dans le crate (format figé `VOXCAP01`) plutôt
  qu'extrait du proxy : garde le tool sans dépendance au crate binaire P0. `// REF:`
  vers `capture.rs` comme autorité du format.

**Vérifié** (tout sous `RUSTFLAGS="-D warnings"`) : `ci/gates.sh`,
`ci/dep-direction.sh`, `cargo fmt --check`, `cargo clippy --workspace
--all-targets`, `cargo test --workspace` — tous verts. cargo-fuzz local : aucun
crash (framing 20.9M, control 4.4M, udp 3.0M, ocb2_decrypt 847k exécutions).

Commits (sur `main`, sans `Co-Authored-By`) : `5fef92f` refs, `153a875` corpus,
`19d8f92` framing, `2c32552` protobuf, `5c8c538` control, `9a5d953` udp,
`0f53017` OCB2, `9846536` fuzzing.

### Phase 2 — proxy MITM oracle (`tools/mitm-proxy`)

Le proxy s'insère entre le client officiel et un vrai Murmur, termine la TLS des
deux côtés, décode/ré-encode **chaque** message (le codec P1 exercé sur du trafic
vivant) et tient un **domaine crypto OCB2 indépendant vers chaque bord** — donc
il déchiffre et re-chiffre la voix, il ne relaie pas la clé. C'est l'oracle
binaire de la roadmap : si le client marche normalement (voix comprise) à travers
le proxy, le framing, la sérialisation, l'enveloppe UDP et la crypto sont corrects
par construction.

| Tranche | Livrable | Fichiers |
|------|----------|----------|
| T1 | Côté **encode** du codec (`encode_frame`/`encode_control`/`encode_udp`, UDPTunnel préservé en octets bruts) : le proxy ré-encode ce qu'il décode | `voxloom-protocol/src/{framing,control,udp}.rs` |
| T2 | **Plan de contrôle** MITM : réassemblage framé, décode/ré-encode, machine à états qui **réécrit `CryptSetup`** (deux `CryptState` par connexion, resyncs absorbés au bord, tout ce qui n'est pas attendu d'un bord est refusé) | `tools/mitm-proxy/src/{relay,session,tls}.rs` |
| T3 | **Ré-encryption UDP** (cœur pur) : décrypte dans le domaine du bord émetteur, valide par round-trip `decode_udp`/`encode_udp`, re-chiffre dans le domaine de l'autre bord ; pings de connectivité non chiffrés passés verbatim ; chaque datagramme finit en une issue explicite (L4) | `tools/mitm-proxy/src/udp.rs` |
| T4 | **Câblage UDP asynchrone** : socket voix client, corrélation **adresse → session**, uplink dédié par client (le serveur distingue les clients), ré-encryption dans les deux sens sur de vraies sockets | `tools/mitm-proxy/src/udp_relay.rs`, `src/{main,relay}.rs` |

Corrélation adresse→session (T4), tracée sur le serveur réel (R1) : un datagramme
d'un pair **connu** utilise le `CryptState` de ce pair ; d'un pair **inconnu**, le
serveur boucle sur les utilisateurs du même IP hôte (`qhHostUsers`) et lie
l'adresse au **premier `checkDecrypt` qui réussit** (`qhPeerUsers`). Les pings non
chiffrés sont répondus **avant** toute association. Fait exploité et vérifié en
source : un `decrypt` OCB2 en échec est **sans effet de bord** (l'IV est restauré,
aucune écriture d'historique de rejeu), donc essayer un datagramme contre plusieurs
domaines candidats ne les corrompt pas — c'est ce qui rend la liaison « au premier
succès » sûre. `// REF:` vers `murmur/Server.cpp` (`Server::run`, `checkDecrypt`).

Choix d'implémentation notables :

- **`std::sync::Mutex` (pas `tokio::sync::Mutex`) pour l'état de session/registre.**
  Les sections critiques ne tiennent **jamais** le lock à travers un `.await` :
  sous le lock il n'y a que du CPU synchrone (OCB2 sur un datagramme = quelques µs,
  lookup `HashMap`, clone d'`Arc`), et les `send`/`recv` awaitent lock relâché.
  C'est l'usage recommandé par tokio et cohérent avec T2 (`Arc<StdMutex<Session>>`
  pour la crypto synchrone, `TokioMutex` pour l'écriture TLS qui, elle, await).
- **Zéro deadlock par construction** : le slot `Option<session>` et le `Session`
  ne sont jamais verrouillés en même temps (`lock_session` clone l'`Arc` puis
  relâche avant de verrouiller le `Session`).
- **`Registry`** (index `IP hôte → sessions`, `qhHostUsers`) publié par le plan
  TCP, consommé par le plan UDP ; désinscription **RAII** à la fin de connexion
  (une session fermée ne lie jamais un datagramme ultérieur).

**Vérifié** (`RUSTFLAGS="-D warnings"`, tout vert) : `ci/gates.sh`,
`ci/dep-direction.sh`, `ci/verifier-boundary.sh`, `cargo fmt --check`, `cargo
clippy --workspace --all-targets`, `cargo test --workspace`.
Done-command de T4 : `cargo test -p voxloom-mitm-proxy` — 9 tests verts, dont
`tests/udp_plane.rs` (relais UDP réel sur loopback : corrélation + ré-encryption
aller/retour, et ping non chiffré traversant avant toute session) et
`tests/corpus_reencrypt.rs` (T3, corpus réel 03–07, 0 rejet).

Commits P2 (sur `main`, sans `Co-Authored-By`) : `1bdb519` T1 encode,
`119bc0a` T2 plan de contrôle, `d06e3bf` T3 ré-encryption UDP, `1077690` T4
câblage UDP async (relais + test `udp_plane` + checklist P2).

### Phase 5 — moteur de vues pur (cœur pur, mergé sur `main` le 2026-07-24)

Deux crates purs de plus (sans IO ni runtime, dépendances gate-conformes) qui
réalisent le pipeline `render → normalize → validate → diff → plan` (spec §12)
comme fonctions pures sur données immuables. Parallélisable avec P2–P4 (aucune
dépendance réseau) ; consomme la roadmap §12.3–12.7, les invariants §20 et
ADR-003/007.

| Crate | Livrable | Fichiers |
|------|----------|----------|
| `voxloom-render` | `ClientView` normalisée + types de vue (§8.3/§8.4) ; `normalize` (§12.3) ; `validate` = invariants §20 vérifiables sur une vue (1, 3, 4, 5, 6, 11, 15) en `Invariant` nommés | `voxloom-render/src/{view,keys,ids,normalize,validate}.rs` |
| `voxloom-reconcile` | `diff` (§12.4, `ViewDelta` de patches champ-à-champ) ; `plan` (§12.5/§12.6, `PlanOp` ordonnés + `OutputTransaction` §12.7) ; `ViewIdMapping` (ADR-007, clé→ID stable, jamais réutilisée, fail-closed) | `voxloom-reconcile/src/{diff,plan,idmap}.rs` |

Le plan est **abstrait** (mutations de vue, pas de messages Mumble) : émettre les
frames wire est le rôle de la couche session (P3). Cela permet de tester l'ordre
contre un applieur de vue pur au lieu d'un client vivant. Ordre de sécurité §12.6
tenu : audio-off avant toute vue (inv 18), audio-on seulement après préparation
de la vue (inv 19) ; parents avant enfants (9), utilisateurs déplacés vers leur
canal final avant toute suppression de canal (8), canaux supprimés enfants avant
parents (10).

- `voxloom-render/src/view.rs` : le masque `PermissionBits` a un layout **canonique**
  (ordre §19.1), **pas** les bits wire de l'ACL Mumble ; le mapping vers `Permission`
  est différé à la session (P3). C'est pourquoi le crate ignore le wire format.
- Générateur déterministe seedé + applieur pur **tiennent lieu** des générateurs
  partagés du testkit et de `SimulatedMumbleClient` (§26.6), qui sont des livrables
  vérificateur de P3 (R2). Propriété tenue sur **4000 seeds** : `apply(committed,
  plan) == desired` après normalisation, aucun état intermédiaire invalide, ordre
  de sécurité respecté. Bug trouvé par la propriété : la suppression d'un canal
  link-référencé par un frère aussi supprimé laissait un lien pendant transitoire ;
  corrigé en modélisant le nettoyage du vrai client.
- Chaque invariant statique porte un test de mutation (échoue si on retire le check).

**Vérifié** (`RUSTFLAGS="-D warnings"`, tout vert) : `ci/gates.sh`,
`ci/dep-direction.sh`, `ci/verifier-boundary.sh`, `cargo fmt --check`, `cargo
clippy --workspace --all-targets`. Done-command : `cargo test -p voxloom-render`
(13 tests) et `cargo test -p voxloom-reconcile` (11 tests : `idmap` 6, `planner`
5 dont le proptest 4000 seeds) ; `cargo test --workspace` vert.

Commits P5 (sans `Co-Authored-By`) : `66b1847` `voxloom-render`, `f6afb72`
`voxloom-reconcile` ; mergés sur `main` le 2026-07-24 (merge no-ff).

### Phase 3 — serveur minimal (close, checkpoint humain signé le 2026-07-26)

Handshake sans Murmur, plan UDP, loopback. Deux crates, en **deux commits
séparés** au titre de R2 (implémenteur ≠ vérificateur, `verifier-boundary.sh`).

| Crate | Rôle | Fichiers |
|------|------|----------|
| `voxloom-server` | Implémenteur : accept TLS (TLS 1.2), handshake, plan UDP, loopback, tunnel TCP | `voxloom-server/src/{tls,config,state,handshake,connection,voice,server,main}.rs` |
| `voxloom-testkit` | Vérificateur (R2) : `SimulatedMumbleClient`, juge strict §20 | `voxloom-testkit/src/{model,client,tls}.rs` |

Ordre du handshake **autoritaire** (R1, tracé dans `handshake.rs`) :
`Server::encrypted` envoie **Version** dès la TLS établie (avant Authenticate) ;
puis sur `Authenticate`, `Messages.cpp::msgAuthenticate` émet **CryptSetup →
CodecVersion → ChannelState (racine, parents avant enfants) → UserState(self) →
UserState(autres) → ServerSync → ServerConfig**. `build_handshake` est une
**fonction pure** : les invariants §20 d'ordre (self avant ServerSync, parents
avant enfants, racine sans parent) sont testés unitairement dessus.

- **Auth stub** : tout username accepté, le password est un credential opaque
  (le flux jeton §10.2 est P8). Nom serveur-autoritaire (§10.5).
- **Plan UDP** (`voice.rs`) : association d'adresse **par preuve cryptographique**
  (`checkDecrypt`, premier décryptage qui réussit ; un décryptage en échec est
  sans effet de bord, sûr à réessayer — hérité P2). Pings chiffrés **et** non
  chiffrés répondus. **Loopback = target 31** (REF `MumbleUDP.proto` : `2^5-1 =
  "server loopback"`) : réfléchi vers l'émetteur, re-chiffré, `context` normal,
  `sender_session` estampillée, Opus intact. Repli **tunnel TCP** (`UDPTunnel`)
  réfléchit le loopback de même. Parole normale (target 0) : **droppée** en P3
  (le graphe de routage est P4).
- **Présence** : arrivée diffusée aux pairs (UserState), départ via UserRemove.
- **`SimulatedMumbleClient`** (§26.6) : applique chaque message serveur à un
  modèle local et **panique** sur toute violation §20, chaque invariant dans son
  propre `check_*` (mutation-testable). Invariants couverts : 1 (racine existe à
  sync), 2 (racine jamais retirée), 3 (parent visible), 4 (pas de cycle), 5
  (user dans canal visible), 6 (self avant ServerSync), 8 (canal occupé jamais
  retiré), 15 (pas d'actor invisible). Écrit contre la spec/référence, parle le
  wire uniquement via `voxloom-protocol` (R2). N'active pas les lints
  workspace (contrat = paniquer bruyamment), évite quand même `.unwrap()`.

Fait par le loopback : voir le débat « le loopback client Mumble est local, mais
le mode Loopback → *Serveur* envoie l'audio avec la cible loopback et attend
l'écho serveur » — c'est exactement le test humain de P3.

**Vérifié** (`RUSTFLAGS="-D warnings"`, tout vert) : `ci/gates.sh`,
`ci/dep-direction.sh`, `ci/verifier-boundary.sh`, `cargo fmt --check`, `cargo
clippy --workspace --all-targets`. Done-commands :
- `cargo test -p voxloom-server` — 8 tests (ordre handshake pur + handshake TLS
  live, association/loopback UDP, loopback tunnel TCP).
- `cargo test -p voxloom-testkit` — 11 tests (9 modèle dont chaque invariant qui
  panique à la violation ; 2 conformance = les deux critères machine de P3).

- **Compteurs OCB2 dans la réponse au `Ping` TCP** (`49bff51`, trouvé pendant le
  checkpoint humain) : la réponse ne portait que le timestamp. Le client lit
  `good` comme `uiRemoteGood` et, s'il vaut encore 0 **20 secondes** après la
  connexion, décide que son UDP ne nous atteint pas et bascule **définitivement**
  en tunnel TCP, alors même que le plan UDP fonctionne dans les deux sens. Sans
  ce report, la case « UDP négocié » de la checklist est inatteignable. `resync`
  vaut 0 en vérité (le resync de nonce est refusé en P3), pas par défaut.
  REF `murmur/Messages.cpp::Server::msgPing`, `mumble/ServerHandler.cpp`
  (`TCPMessageType::Ping`).

Commits P3 (sur `main`, sans `Co-Authored-By`, **séparés R2**) : `4465149`
`voxloom-server` (implémenteur), `e56caf1` `voxloom-testkit` (vérificateur),
`49bff51` compteurs `Ping` (correctif du checkpoint humain).

**Checkpoint humain : signé** (`docs/checklists/p3-minimal-server.md`, 2026-07-26,
serveur `49bff51`). Validé sur un vrai client Mumble : connexion sans Murmur,
handshake propre, UDP négocié, loopback serveur audible sans artefact en UDP et
en repli « Force TCP », deux clients réels qui se voient, déconnexion propre.
Réserve confirmée et attendue : deux clients réels ne s'entendent **pas** entre
eux (cible 0 droppée faute de graphe de routage), seul le loopback cible 31 est
réfléchi. C'est P4.

### Phase 4 — routage audio (T1–T5 faits, T6 + checkpoint humain restants)

Le hot path dans sa forme conceptuelle définitive, avec un contenu trivial. Le
snapshot dit « tout le monde entend tout le monde », mais il est publié par le
mécanisme final : remplacer le corps de `compile` en P7/P8 ne touche rien d'autre.

| Tranche | Livrable | Fichiers |
|------|----------|----------|
| T1 | `voxloom-audio` **pur** : `compile` (chemin froid), `AudioRoutingSnapshot`/`RouteMatrix`/`SenderMetadata` (§15.3), `may_receive` (§15.4), `AudioTarget`/`AudioContext` (§15.5), `outgoing_audio` (§15.2) | `voxloom-audio/src/{snapshot,policy,envelope}.rs` |
| T2 | Câblage serveur : snapshot recompilé dans la section critique qui change l'appartenance, lu par clone d'`Arc` (le swap atomique de §23.2, zéro dépendance), routage cible 0, chiffrement par destinataire | `voxloom-server/src/{routing,state,voice}.rs` |
| T3 | Parité inter-transport : les deux ingress convergent sur la même fonction, chaque destinataire est joint sur le transport qu'il a lui-même utilisé en dernier | `voxloom-server/src/{connection,routing}.rs` |
| T4 | Limites §15.7 : bande de taille 2..=1024 o (référence) appliquée aux deux ingress, token bucket de paquets voix par connexion (horloge injectée) | `voxloom-server/src/limits.rs` |
| T5 | **Vérificateur (R2)** : plan voix du client simulé + test de charge N clients, zéro perte interne, latence bornée | `voxloom-testkit/src/client.rs`, `tests/voice_load.rs` |

Règle de transport, tracée en source (R1) : `aiUdpFlag` vaut 1 à la construction,
passe à 0 quand le client tunnelise de l'audio, revient à 1 quand un datagramme
de lui arrive ; l'émission choisit l'UDP seulement si le drapeau est mis **et**
que le pair a une adresse prouvée. Un client dont l'UDP meurt en cours d'appel
continue donc d'entendre tout le monde, par le tunnel. REF `Server::sendMessage`,
`Server::run`, `ServerUser.cpp`.

- **Le loopback n'est plus un cas particulier** : l'émetteur est stocké en tête
  de sa propre plage de destinataires, donc la cible 31 est une route ordinaire
  à un élément. Le comportement validé en P3 est préservé à l'identique.
- **Cibles shout/whisper refusées et journalisées** plutôt que routées comme de
  la parole normale, ce qui livrerait de la voix à des auditeurs que le client
  n'a jamais adressés. Leur enregistrement par `VoiceTarget` est P9.
- **Défaut trouvé par une propriété** (T1) : collapser les participants
  dupliqués en « le premier gagne » faisait dépendre le domaine survivant de
  l'ordre d'entrée, donc une même session pouvait compiler vers deux partitions
  différentes. Une session revendiquée par deux domaines n'est désormais routée
  par **aucun** : perdre de l'audio vaut mieux que le livrer dans la mauvaise
  partition. C'est exactement la classe de bug que P7/P8 auraient héritée.

**Vérifié** (`RUSTFLAGS="-D warnings"`, tout vert) : `ci/gates.sh` (dont les
gates `voxloom-audio` : ni verrou, ni await, ni callback, ni accès à l'état),
`ci/dep-direction.sh`, `ci/verifier-boundary.sh`, `cargo fmt --check`, `cargo
clippy --workspace --all-targets --all-features`. Done-commands :
- `cargo test -p voxloom-audio` — 16 tests (propriétés sur 4000 seeds + smoke).
- `cargo test -p voxloom-server` — 12 tests, dont 4 d'intégration de routage
  (deux clients en UDP, tunnel→UDP, UDP→tunnel, cible non enregistrée).
- `cargo test -p voxloom-testkit` — 13 tests, dont le test de charge (4 clients,
  25 rondes, 300 livraisons, zéro perte).
- `ci/fuzz-smoke.sh 20` — 7 cibles, `audio_route` incluse (3,87 M exécutions).

Commits P4 (sans `Co-Authored-By`, **séparés R2**) : `3fbe7b2` `voxloom-audio`,
`68596d2` `voxloom-server` (implémenteur), `650e0f5` `voxloom-testkit`
(vérificateur).

**Reste pour clore P4** : (1) le bench criterion coût par paquet et par
destinataire, transformé en test de non-régression (T6) ; (2) le point de
contrôle humain — deux vrais clients qui s'entendent, et le repli TCP vérifié en
coupant l'UDP. La checklist reste à écrire.

---

## Reste à faire

### 0. ~~Point de contrôle humain P2~~ (fait, signé le 2026-07-24)

**Résolu.** `docs/checklists/p2-proxy-oracle.md` est déroulé et signé (commit
`2fb1e98`) : un vrai client Mumble parle à travers le proxy sans artefact
audible, dans les deux sens, deux clients, resync et déconnexion propre. Un crash
du client macOS au handshake a été diagnostiqué (introspection TLS 1.3 de Qt) et
corrigé (proxy épinglé sur TLS 1.2). P2 est close.

### 1. ~~Capturer un corpus contre un serveur ≥ 1.5.0~~ (fait, déjà dans le corpus)

**Résolu.** Les scénarios 03–07 tapent déjà un serveur 1.5.857 et exercent le
chemin UDP protobuf de `decode_udp` sur trafic réel (voir « Nature du corpus » en
tête de doc). Aucune capture supplémentaire n'est requise pour valider ce chemin.
Diversifier resterait *possible* mais non nécessaire (autres versions ≥ 1.5,
positional data non vide, whisper/voice-target réellement exercé — cf. réserve
`05-whisper`/`06-permission-denied` pour les golden tests de P3). Toute nouvelle
capture reste une tâche P0, PR séparée (R2), point de contrôle humain.

### 2. Job nightly cargo-fuzz en CI (différé, optionnel)

`ci/fuzz-smoke.sh` existe et tourne en local. L'intégrer comme **job nightly**
dans `.github/workflows/ci.yml` (toolchain nightly + `cargo install cargo-fuzz`
ou `taiki-e/install-action@cargo-fuzz`, budget court). Différé car non
vérifiable en local (GitHub Actions). Le smoke stable (`*/tests/fuzz_smoke.rs`)
couvre déjà le contrat « aucune panique » dans la CI stable existante.

### 3. Clôture de P5 (dépend de P3 et P7)

Le cœur pur de P5 est fait (voir « Fait »), mais ces restes exigent d'autres
phases et n'ont **pas** été inventés ici (R1/R5) :

- **Proptest autoritaire à grand volume (>10⁵ paires, CI nocturne) contre le
  client simulé.** `SimulatedMumbleClient` (§26.6) existe désormais dans
  `voxloom-testkit` (livré en P3, R2). Le proptest de P5 peut maintenant le
  consommer ; le proptest in-crate actuel (4000 seeds, générateur + applieur
  locaux) tient le même contrat en attendant le déplacement. Note : le client
  simulé actuel modélise le handshake/présence (§20 : 1-6, 8, 15) ; le pilotage
  d'un plan de transition complet (appliquer un `OutputTransaction` de
  `voxloom-reconcile`) reste à câbler côté testkit pour ce proptest.
- **`render_full` depuis `CanonicalState`.** L'état canonique est **P7** ; le
  moteur opère pour l'instant sur des `ClientView` déjà résolues. Le rendu depuis
  composants (§8.1, spécifique Minecraft) est **P8**.
- **Invariants hors périmètre du moteur pur** : 7 (session audio sortante = P4),
  13/14/17 (chemin commande entrante = P3/P9). Notés, pas implémentés.
- **Cibles fuzz `normalize`/`planner`** (testing.md) : à ajouter avec le job
  nightly (point 2), même contrat « aucune panique ».
- **Limite documentée** : le planificateur suppose des reparentages sans cycle
  transitoire (tenu quand les IDs parents sont monotones, ce que produit
  `ViewIdMapping`) ; le reparentage général façon Mumble est une tâche future.

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
- **Lecteur `.voxcap`** : deux implémentations volontairement séparées
  (`tools/recording-proxy/src/capture.rs`, autorité du format ; réimplémenté dans
  `tools/corpus-decode`). Format figé `VOXCAP01` — si tu le changes, bouge le magic
  et les DEUX lecteurs ensemble.
- **Références vendorées** : `corpus-decode` a nécessité des extraits **au-delà**
  de `references/vendored/` (handler `CryptSetup` client/serveur, `decodePing_legacy`,
  chemin UDP du serveur). Ils viennent du clone pinné `references/mumble/` (non
  commité, gitignore ; reproductible via `references/mumble.pin` + PROVENANCE.md).
  Si un fait doit devenir permanent, le vendorer explicitement.
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
tools/corpus-decode/      binaire P1 (décodeur de corpus, critère de « done »)
tools/mitm-proxy/         binaire P2 (proxy MITM oracle : TLS deux bords,
                          réécriture CryptSetup, ré-encryption UDP, relais async)
voxloom-protocol/         framing, messages prost, control, udp (decode + encode) (pur)
voxloom-crypto/           ocb2 (OCB2-AES128, CryptState)          (pur)
voxloom-render/           P5 : vue normalisée, normalize, validate (§20)  (pur)
voxloom-reconcile/        P5 : diff, planificateur (§12.5/12.6), ViewIdMapping (pur)
voxloom-audio/            P4 : routage pur (compile, may_receive, enveloppe)  (pur)
voxloom-server/           P3+P4 : serveur minimal + routage voix, limites §15.7
voxloom-testkit/          P3+P4 : SimulatedMumbleClient (§20 + plan voix), juge (R2)
fuzz/                     cibles cargo-fuzz (workspace détaché, nightly)
references/vendored/      vérité protocolaire (R1), pin v1.5.915
fixtures/corpus/          7 captures réelles (zone vérificateur R2)
```

Note : `voxloom-render`/`voxloom-reconcile` sont sur `main` (P5 mergé le
2026-07-24). `voxloom-server`/`voxloom-testkit` sur `main` (P3, le 2026-07-24).

### Après P4

Le cœur de P4 est fait et vert (T1–T5, voir « Fait »). Restent le bench criterion
(T6) et le point de contrôle humain. Ensuite **P6 (vues par connexion en live)**,
qui branche le moteur pur de P5 sur les connexions réelles. P5 peut aussi être
clôturé en branchant son proptest sur le `SimulatedMumbleClient`, qui sait
désormais aussi juger le plan voix (cf. « Reste à faire » §3). Voir la roadmap.

Pièges P4 à retenir : (1) les gates `audio/no-render-dep` et `audio/no-state-dep`
grep les **chaînes** `voxloom[_-]render` / `voxloom[_-]state` sur tout
`voxloom-audio/src`, **commentaires compris** — mentionner ces crates dans une
doc, même pour expliquer qu'on n'en dépend pas, casse la CI ; formuler en
concepts (« l'état vivant », « l'état canonique ») ; (2) le chiffrement par
destinataire impose une allocation par destinataire (sortie OCB2 + enveloppe),
inhérente à « chiffrer séparément pour chaque destinataire » (§15.2) — c'est ce
que T6 doit mesurer avant toute optimisation (§27.4).

Pièges P3 à retenir : (1) se connecter via `127.0.0.1`, pas `localhost` (IPv6) ;
(2) TLS épinglé 1.2 (crash client macOS en 1.3) ; (3) la séparation R2 se fait en
**commits distincts** (`voxloom-server/src` vs `voxloom-testkit/`) — jamais dans
le même diff ; (4) le testkit n'active pas les lints workspace mais évite quand
même `.unwrap()` (gate global) ; (5) **toute réponse au `Ping` TCP doit reporter
les compteurs OCB2** (`good`/`late`/`lost`/`resync`) : un `good` à zéro fait
basculer le vrai client en tunnel TCP définitif au bout de 20 s, en silence, sans
qu'aucun test local ne le voie. Le certificat auto-signé étant régénéré à chaque
lancement, le client repart d'une ardoise vierge à chaque redémarrage du serveur
(il indexe cette décision par `sha1(clé publique)`), ce qui masque le symptôme en
test court.
