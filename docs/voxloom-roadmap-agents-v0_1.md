# Voxloom : roadmap d'implémentation orientée agents

**Statut :** roadmap v0.1, complément de la spécification technique v0.1
**Public :** agents IA d'implémentation, avec points de contrôle humains explicites
**Principe directeur :** un agent ne peut pas juger la conformité protocolaire. Seul un oracle le peut. Toute la roadmap est ordonnée pour construire les oracles avant le code qu'ils jugent.

---

## 0. Règles du jeu pour les agents

Ces règles s'appliquent à toutes les phases. Elles existent parce que les modes d'échec des agents sont connus : halluciner un détail protocolaire plausible, affaiblir un test pour le faire passer, introduire une abstraction non demandée, contourner une interdiction structurelle "juste pour ce cas".

### R1. La vérité protocolaire ne vient jamais de mémoire

Toute affirmation sur le wire format doit être traçable vers une source vendored dans le dépôt : `references/mumble/` (clone pinné du dépôt mumble-voip), le corpus de captures (`fixtures/corpus/`), ou la spécification Voxloom. Un agent qui a besoin d'un détail absent de ces sources s'arrête et le signale au lieu de l'inventer. Chaque module protocolaire contient un commentaire `// REF:` pointant vers le fichier source Mumble correspondant.

### R2. Séparation implémenteur / vérificateur

Les répertoires `voxloom-testkit/`, `fixtures/`, et `conformance/` sont modifiables uniquement par des tâches de type "vérification", jamais par une tâche d'implémentation. Un agent d'implémentation dont les tests échouent corrige l'implémentation, pas le test. Toute modification d'un test de conformité passe par une revue humaine. Enforcement : CI refuse un diff qui touche à la fois `voxloom-*/src` et `conformance/`.

### R3. Critère de done machine-vérifiable

Chaque tâche se termine par une commande exacte qui doit passer (`cargo test -p ...`, `cargo fuzz run ... -- -max_total_time=300`, script de scénario). "Ça compile" et "ça a l'air correct" ne sont pas des critères. Une tâche sans commande de done est une tâche mal spécifiée : la refuser.

### R4. Interdictions structurelles en CI dès le jour 1

Gates grep/clippy actifs avant la première ligne de logique :

```text
voxloom-audio/src :
  interdits : Mutex, RwLock, .await dans le chemin par-paquet,
              Box<dyn Fn, appels vers voxloom-render ou un flavor
voxloom-render/src :
  interdit d'importer voxloom-protocol (le renderer ignore le wire format)
voxloom-flavor/src :
  interdits : types protocolaires Mumble et concepts d'un flavor concret
voxloom-protocol, voxloom-crypto :
  interdits : tokio, IO ; crates purs, sans dépendance runtime
tout le workspace :
  interdits : unwrap() hors tests, static mut, unsafe sans commentaire SAFETY
```

La direction des dépendances entre crates est vérifiée par un script CI sur `cargo metadata`. Ces gates encodent les ADR 001, 002 et 005 : si un agent doit les contourner, l'architecture a un problème, pas le gate.

### R5. Tranches verticales, binaire démontrable

Chaque phase se termine par un binaire ou un script qu'un humain peut lancer. Pas de phase "que des types". Les abstractions non exigées par la phase courante sont interdites : la spec liste les crates cibles, mais un crate n'existe que quand une phase le remplit.

### R6. Fail closed par défaut

Tout chemin non implémenté répond par un refus explicite (`PermissionDenied`, `Reject`, drop journalisé), jamais par un succès silencieux. Un `todo!()` qui pourrait être atteint par un client est un bug de sévérité maximale.

---

## Phase 0 : infrastructure de vérité

**Objectif :** rendre le protocole observable et testable avant d'écrire du code qui le parle. C'est la phase la plus importante de la roadmap : tout le reste consomme ses artefacts.

Livrables :

1. Workspace cargo avec les gates R4 actifs, CI verte sur un workspace vide.
2. `references/mumble/` : clone pinné (commit hash fixé) du dépôt mumble-voip. Extraction de `Mumble.proto`, `MumbleUDP.proto`, et des vecteurs de test de `CryptState` (les tests OCB2 du dépôt officiel deviennent des fixtures).
3. **Corpus de captures** : transcripts binaires de sessions réelles client officiel ↔ Murmur, via un proxy TCP/UDP d'enregistrement (à écrire, trivial : il ne décode rien, il journalise des octets horodatés et annotés par direction). Scénarios à capturer : handshake complet, join/leave de canal, création/suppression de canal, deux clients qui parlent, whisper, permission denied, déconnexion, resync crypto si provocable.
4. Décision gelée : format UDP legacy supporté ou non. Elle conditionne la structure de `voxloom-protocol` et ne doit pas rester ouverte. Recommandation : legacy requis si un client Android doit se connecter un jour, et le corpus doit alors inclure une session Mumla.

**Humain requis :** installer Murmur et le client officiel, dérouler les scénarios de capture à la main (un agent ne pilote pas une GUI Qt), trancher la décision legacy UDP.

**Done :** `fixtures/corpus/` contient au moins 8 scénarios annotés ; un README décrit chaque scénario ; le hash du commit mumble de référence est fixé dans un fichier versionné.

---

## Phase 1 : codec pur (`voxloom-protocol`, `voxloom-crypto`)

**Objectif :** encoder/décoder tout le corpus, sans IO. Crates purs, donc territoire idéal pour les agents : entrées/sorties définies, oracle disponible, fuzzing immédiat.

Tâches :

1. Framing TCP (type u16 BE, longueur u32 BE, payload), avec parsing incrémental sur buffers partiels et limites de taille strictes.
2. Messages Protobuf (génération depuis le `.proto` vendored via prost, jamais retapés à la main).
3. Cas spécial `UDPTunnel` : payload = octets audio bruts, pas le message Protobuf déclaré. Test dédié qui échoue si quelqu'un "corrige" ça un jour.
4. Formats UDP : protobuf (1.5+) et, si décidé en Phase 0, legacy (header varint, types audio, ping). Décodage d'enveloppe sans décodage Opus.
5. OCB2 : implémentation validée contre les vecteurs extraits du dépôt officiel, y compris les mitigations spécifiques de Mumble contre les attaques connues, reproduites à l'identique. Module isolé, sans dépendance sur le reste.
6. Fuzzing cargo-fuzz sur : framing, protobuf, enveloppes UDP, crypto. Intégré en CI (budget temps court par PR, runs longs nocturnes).

**Propriétés testées :** roundtrip encode∘decode = id sur messages générés ; decode total du corpus Phase 0 sans erreur ni octet inexpliqué ; aucune panique sous fuzzing.

**Done :** un binaire `corpus-decode` relit chaque capture et produit un transcript lisible complet ; `cargo test` + fuzz smoke verts.

---

## Phase 2 : proxy MITM comme oracle vivant

**Objectif :** prouver le codec et la crypto contre le client réel avant d'écrire un serveur. Le proxy s'insère entre client officiel et Murmur, décode chaque message TCP et chaque datagramme, le réencode, et le transmet. Il termine la TLS des deux côtés et re-chiffre l'UDP avec ses propres états OCB2 vers chaque extrémité.

Pourquoi cette phase existe : si le client fonctionne normalement à travers le proxy (voix comprise), alors le framing, la sérialisation, l'enveloppe UDP et la crypto sont corrects par construction. C'est un oracle binaire, visible, non truquable par un agent, et il ne demande aucune sémantique serveur.

**Humain requis :** lancer le client, parler, vérifier l'audio dans les deux sens, dérouler les scénarios du corpus à travers le proxy.

**Done :** appel vocal complet à travers le proxy sans artefact audible ; transcript du proxy identique en structure aux captures de référence ; checklist humaine signée dans le dépôt.

---

## Phase 3 : serveur minimal (Phase 0-1 de la spec)

**Objectif :** handshake sans Murmur. TLS accept, `Version`, `Authenticate` (jeton accepté en mode stub), `CryptSetup`, root channel, self user, `ServerSync`, `ServerConfig`, ping TCP/UDP, association UDP par preuve cryptographique, loopback audio, fallback tunnel TCP.

En parallèle et par un agent distinct (R2) : `SimulatedMumbleClient` dans le testkit. Modèle strict qui applique les messages serveur à un état local et **panique** sur toute violation des invariants de la section 20 de la spec (canal inconnu référencé, parent manquant, self absent avant ServerSync, ID dupliqué...). Le client simulé est le juge de toutes les phases suivantes ; il doit être écrit contre le corpus et la référence, pas contre le serveur Voxloom.

La séquence initiale est validée par golden test contre le corpus : l'ordre exact émis par Murmur fait foi en cas de doute (notamment la position de `CodecVersion` par rapport à `ServerSync`).

**Done :** client officiel connecté, loopback fonctionnel (humain) ; le client simulé rejoue le handshake sans panique (CI) ; deux clients simulés connectés simultanément avec vues indépendantes en mémoire.

---

## Phase 4 : routage audio deux clients

**Objectif :** le hot path, dans sa forme finale conceptuelle même si le contenu est trivial. Pipeline complet de la section 15.1 : association crypto, déchiffrement, anti-rejeu, enveloppe, validation session/target, rate limit, consultation d'un `Arc<AudioRoutingSnapshot>` chargé par swap atomique, réécriture des métadonnées, chiffrement par destinataire, émission. Snapshot codé en dur ("tout le monde entend tout le monde") mais publié par le mécanisme définitif.

Les types portent `RoutingDomainId` dès maintenant, même avec un seul domaine : le partitionnement doit être structurel avant d'être utile (cf. review de la spec, décision à figer en ADR-011/012).

Gates R4 pleinement actifs : le routeur ne contient ni lock ni await par paquet ni appel hors de son crate. Un bench criterion mesure le coût par paquet par destinataire et devient un test de non-régression.

**Done :** deux clients officiels s'entendent (humain) ; test de charge testkit : N clients simulés, débit soutenu, zéro perte interne, latence routeur bornée (CI) ; fallback TCP vérifié en coupant l'UDP.

---

## Phase 5 : moteur de vues pur (render, normalize, diff, plan)

**Objectif :** le cœur différenciant, implémenté comme fonctions pures sur données immuables, sans réseau. C'est la deuxième zone idéale pour agents : tout est propriété testable.

Contenu : `ClientView` normalisée, `render_full`, normalisation (section 12.3), diff logique (12.4), planificateur de transitions (12.5) avec les règles d'ordre de sécurité (12.6 : audio avant vue pour une interdiction, vue avant audio pour une autorisation), transaction de sortie (12.7), mapping clé→ID par connexion avec stabilité (ADR-007).

Property tests, exécutés contre le client simulé de Phase 3 :

```text
∀ (old, new) vues valides générées :
  le plan produit par le planner, appliqué au client simulé partant de old,
  aboutit exactement à new,
  sans violer aucun invariant intermédiaire de la section 20,
  et les 20 invariants sont chacun encodés comme assertion nommée.
```

Fuzzing sur la normalisation et le planner. Le rendu incrémental n'existe pas encore : full render uniquement, conformément à ADR-003 et à la section 27.4 (pas d'optimisation avant mesure).

**Done :** proptest à grand volume (>10^5 paires) vert en CI nocturne ; chaque invariant a au moins un test qui échoue si on le retire du validateur (tests de mutation manuels sur le validateur).

---

## Phase 6 : vues par connexion en live (Phase 2 de la spec)

**Objectif :** brancher le moteur de Phase 5 sur les connexions réelles. Canaux synthétiques, vues divergentes par viewer, modification sans reconnexion, reconnexion forcée sur divergence grave (ADR-009).

C'est ici que se vérifie l'hypothèse la plus originale et la moins prouvable par agent : le comportement du client officiel face à des vues dynamiques (IDs par connexion, caches locaux par Channel ID, préférences par certificat, churn visuel). Prévoir une checklist humaine explicite : renommages à chaud, déplacement d'utilisateurs entre canaux synthétiques, reconnexion avec IDs déterministes, vérification que les volumes/surnoms locaux survivent.

**Done :** critère de la spec Phase 2 : Alice et Bob voient des arbres différents et divergents à chaud, sans reconnexion (humain) ; scénarios équivalents rejoués par clients simulés en CI ; test "aucun message émis ne référence une entité hors vue du destinataire" actif sur toutes les sorties.

---

## Phase 7 : intégration de flavors et publication atomique

**Décision préalable :** `docs/decisions/0002-flavor-owns-business-state.md`.
Voxloom ne possède jamais l'état métier. Le flavor compilé possède son snapshot,
ses commandes, ses acteurs et ses transactions. Le runtime ne connaît que les
connexions vocales, les vues engagées, les routes audio et les générations
publiées.

**Objectif :** introduire le contrat `VoiceFlavor`, le coordinateur de
publication `voxloom-control` et un binaire de composition. Un flavor fournit
un snapshot métier immuable et des sorties déclaratives. Voxloom traite le
snapshot comme opaque, valide les sorties et publie les transitions de vue et
le snapshot audio dans l'ordre de sécurité.

### T1. Contrat minimal de flavor

Créer `voxloom-flavor` avec les types génériques `VoiceFlavor`,
`FlavorRevision`, `RenderOutput` et `FlavorError`. L'API est statique, sans ABI
dynamique, callback dans le hot path ni concepts joueur, realm, équipe, position
ou radio.

**Done :** `cargo test -p voxloom-flavor`.

### T2. Publication d'un snapshot opaque

Créer le chemin `Arc<F::Snapshot>` vers rendu complet de toutes les connexions.
Le snapshot et sa révision restent figés pendant toute la génération. Voxloom ne
lit le snapshot qu'en appelant le flavor et ne conserve aucun état métier
dérivé comme source de vérité.

**Done :** `cargo test -p voxloom-control snapshot_publication`.

### T3. Validation des sorties du flavor

Valider séparément la vue désirée, les routes audio et le registre
d'interactions avant tout effet. Une erreur de flavor ou une sortie invalide
annule toute la génération et conserve la génération engagée.

**Done :** `cargo test -p voxloom-control flavor_output_validation`.

### T4. Publication atomique avec ordre de sécurité

Attribuer une génération Voxloom monotone et produire les transactions de vue
et le snapshot audio. Une révocation audio devient effective avant le retrait
visuel ; une nouvelle route n'est activée qu'après le commit de la vue du
destinataire.

**Done :** `cargo test -p voxloom-control publication_order`.

### T5. Événements vocaux vers l'intégration

Convertir les actions Mumble déjà résolues dans la vue courante en
`VoiceEvent` versionnés. Le flavor décide seul des mutations métier. Il peut
ensuite publier un nouveau snapshot, mais Voxloom n'applique jamais de commande
métier.

**Done :** `cargo test -p voxloom-control voice_events`.

### T6. Flavor de référence Aurora/Borealis

Extraire le modèle déterministe utilisé en P6 dans un crate de flavor de
référence. Il possède ses realms et ses mutations, puis rend les mêmes vues et
routes à travers l'API publique. Aucun crate central ne dépend de ce crate.

**Done :** `cargo test -p voxloom-flavor-reference`.

### T7. Vérificateur de confidentialité

Dans une tâche R2 séparée, générer des paires de snapshots du flavor de
référence et appliquer chaque génération avec le client simulé. Vérifier
qu'aucune sortie ne référence une entité absente de la vue du destinataire, sur
tous les canaux de la section 26.7. Ajouter le scénario de révocation sous
charge audio qui échoue si un paquet traverse entre deux générations.

**Done :** `cargo test -p voxloom-testkit --test flavor_privacy`.

### T8. Binaire de composition et checkpoint

Ajouter un binaire qui compile explicitement le flavor de référence avec le
runtime. Rejouer Aurora/Borealis sans branche métier dans `voxloom-server`,
mesurer le coût d'une publication complète, puis valider sur deux clients
officiels qu'un changement de snapshot conserve les propriétés observées en P6.
Une fois l'extraction faite, étendre les gates pour empêcher le retour de
concepts du flavor de référence dans les crates centrales.

**Done :** `ci/bench-publication.sh` et checklist humaine
`docs/checklists/p7-flavor-integration.md` signée.

**Done de phase :** T1 à T8 sont verts ; le flavor de référence reproduit P6 à
travers l'API publique ; les crates centrales ne contiennent aucun concept
métier ; le proptest de confidentialité et le scénario de révocation sont
verts ; le benchmark mesure la publication et le rerender complet de N
connexions pour un changement de snapshot.

---

## Phase 8 : flavor Minecraft

**Objectif :** implémenter un flavor Minecraft au-dessus de l'API P7 et un
binaire de composition qui le compile avec Voxloom. Le flavor possède les
jetons à usage unique (10.2, consommation atomique, entropie, expiration),
l'association certificat vers principal, l'état joueur, les parties, équipes,
dimensions, positions autoritaires et règles de proximité.

Les acteurs par partie, `MergeRealms`, `SplitRealm`, les changements d'équipe et
le batching des événements Minecraft vivent dans ce flavor. Ils produisent des
snapshots immuables consommés par Voxloom ; aucune crate centrale ne dépend de
Minecraft.

Les positions suivent le chemin de la section 13.7 : hors VDOM, index spatial, recompilation du snapshot par tick batché (50-100 ms) et par domaine de routage, avec hystérésis sur les seuils de distance pour éviter le flapping des routes. Le VDOM ne voit un joueur bouger que si la structure change (changement de dimension, d'équipe), jamais à chaque déplacement.

**Done :** critère spec Phase 4 : plusieurs parties isolées sur un endpoint unique, proximité fonctionnelle, changements sans reconnexion (humain + scénarios simulés) ; bench : M joueurs à 20 Hz de positions, recompilation par tick sous budget, aucune pression sur le control plane.

---

## Phase 9 : interactions et durcissement

Regroupe les Phases 5-6 de la spec : permissions effectives, context actions avec revalidation par génération (24.4), texte contrôlé, voice targets validés, radios, listeners traduits ou refusés, `explain_audio` et inspecteur de vues (25.2-25.3), rate limits complets (22.5), replay des publications de flavor et événements vocaux, fuzzing continu élargi (commandes client, résolution d'IDs), campagne de compatibilité clients réels (desktop x3 OS, mobile selon décision Phase 0), tests longue durée.

**Done :** les 15 critères MVP de la section 33, chacun mappé à un test ou une checklist humaine identifiée ; matrice de compatibilité clients remplie et versionnée.

---

## Phase 10 : optimisation (conditionnelle)

Uniquement après profiling (27.4) : invalidation ciblée des connexions avec
fallback `All`, cache de sous-arbres, dependency tracking, retained-mode sur les
profils mesurés coûteux. Oracle déjà en place depuis la Phase 5 :
`normalize(render_incremental) == normalize(render_full)` en proptest, plus
comparaison aléatoire d'une fraction des rendus en mode dev. Un agent n'entame
cette phase que sur présentation de mesures, pas d'intuition.

---

## Récapitulatif des dépendances

```text
P0 corpus/refs ──> P1 codec ──> P2 proxy oracle ──> P3 serveur minimal ──> P4 hot path
                        │                                  │
                        └──> P5 moteur de vues pur <───────┘ (client simulé)
                                      │
                                      ▼
                              P6 vues live ──> P7 API flavor ──> P8 flavor Minecraft ──> P9 durcissement ──> P10 opti
```

P5 peut démarrer en parallèle de P2-P4 (aucune dépendance réseau). P1 et le client simulé de P3 sont les deux chantiers agents les plus parallélisables.

## Points de contrôle humains (résumé)

```text
P0 : captures corpus, décision legacy UDP
P2 : validation audio à travers le proxy
P3 : premier handshake client officiel
P4 : premier appel deux clients
P6 : checklist comportement client sur vues dynamiques
P9 : campagne de compatibilité multi-clients
+ toute modification de conformance/ ou du testkit (R2)
```
