# Mumble Server Runtime

## Spécification technique d’un runtime vocal déclaratif compatible Mumble

**Statut :** spécification protocolaire et historique d’architecture, version 0.1
**Langage d’implémentation ciblé :** Rust  
**Compatibilité ciblée :** clients Mumble standards, avec une première cible recommandée Mumble 1.5+ et Opus uniquement  
**Nature du projet :** nouvelle implémentation serveur compatible avec le protocole Mumble, sans chercher à reproduire la sémantique interne de Murmur

> **Amendement d'architecture :** la décision
> `docs/decisions/0002-flavor-owns-business-state.md` précise que l'état métier
> appartient au flavor compilé, pas au runtime Mumble Server Runtime. Le terme historique
> `CanonicalState` désigne ici le snapshot d'un flavor, jamais un modèle imposé
> par le cœur.
>
> **Topologie courante :** les sections qui découpent l'implémentation en crates
> P4–P7 décrivent l'architecture exploratoire conservée au tag
> `legacy-p7-final`. Le runtime actif est `voxloom-shard` +
> `voxloom-gateway`, selon `docs/design/guide-implementation.md`. Les exigences
> protocolaires et les invariants de ce document restent applicables.

---

## 1. Nommage

### 1.1 Nom de travail retenu : Mumble Server Runtime

**Mumble Server Runtime** combine :

- _vox_, la voix ;
- _loom_, le métier à tisser ;
- l’idée qu’un snapshot métier fourni par un flavor est « tissé » en vues,
  relations d’audibilité et interfaces différentes pour chaque connexion.

Tagline proposée :

> **A declarative Mumble-compatible voice runtime.**

Le nom reste assez abstrait pour ne pas enfermer le projet dans Minecraft, tout
en reflétant son principe fondamental : construire des environnements vocaux
projetés à partir d'un snapshot métier opaque fourni par une intégration.

### 1.2 Autres noms envisageables

| Nom                | Idée                                                                                          |
| ------------------ | --------------------------------------------------------------------------------------------- |
| **Murmuration**    | Un groupe qui se divise, fusionne et se recompose dynamiquement. Clin d’œil discret à Murmur. |
| **Echoform**       | La forme visible et audible produite à partir d’un état abstrait.                             |
| **Parallax Voice** | Chaque observateur reçoit une vue différente du même monde.                                   |
| **Auralattice**    | Un réseau de relations vocales plutôt qu’une arborescence de salons.                          |
| **Manyfold**       | Une vérité canonique, plusieurs projections.                                                  |
| **Voxweave**       | Variante plus explicite de Mumble Server Runtime, un peu moins distinctive.                   |

Le reste du document utilise **Mumble Server Runtime** comme nom de travail.

---

## 2. Résumé exécutif

Mumble Server Runtime est une nouvelle implémentation de serveur compatible avec le protocole Mumble. Il ne cherche pas à reproduire Murmur, ses serveurs virtuels, ses ACL historiques ou son modèle « un utilisateur appartient à un canal partagé par tous ».

Mumble Server Runtime ne conserve pas l'état métier. Un **flavor compilé** possède cet état et
publie des snapshots immuables, par exemple pour une application Minecraft :

- joueurs Minecraft ;
- mondes et dimensions ;
- parties ;
- équipes ;
- rôles ;
- radios ;
- positions autoritaires ;
- permissions ;
- relations de visibilité ;
- relations d’audibilité.

Pour chaque connexion Mumble, le flavor calcule des sorties déclaratives que
Mumble Server Runtime valide et publie :

1. une **vue client**, contenant les canaux, utilisateurs, permissions et actions que le client doit afficher ;
2. un **graphe d’audibilité**, indiquant quels flux audio cette connexion peut recevoir ;
3. un **registre d’interactions**, permettant de traduire les actions du client en intentions métier ;
4. éventuellement des **effets ponctuels**, comme un message, un refus ou une déconnexion.

Le modèle général est :

```text
Snapshot métier opaque + contexte vocal
                  │
                  └── flavor.render(connection)
                            └── RenderOutput
                                  ├── DesiredClientView
                                  ├── DesiredAudioRoutes
                                  └── InteractionRegistry
```

Pour chaque connexion :

```text
CommittedClientView
        +
DesiredClientView
        │
        ▼
Réconciliation
        │
        ▼
Plan de transition valide
        │
        ▼
Messages Mumble ordonnés
```

Le client Mumble devient un terminal de rendu :

- `ChannelState` crée ou modifie un canal ;
- `ChannelRemove` retire un canal ;
- `UserState` crée ou modifie un utilisateur ;
- `UserRemove` retire un utilisateur ;
- `PermissionQuery` renseigne les permissions effectives ;
- `ContextActionModify` ajoute ou retire des interactions ;
- les paquets audio sont transmis selon le graphe d’audibilité calculé par Mumble Server Runtime.

La structure visible n’a aucune obligation de correspondre au routage audio. Deux utilisateurs peuvent être affichés dans le même canal sans s’entendre, ou dans des canaux différents tout en s’entendant. Un client doit seulement connaître la session d’un émetteur avant de recevoir son audio.

Mumble Server Runtime est donc plus proche de **Minestom pour Mumble** que d’un remplacement direct de Murmur : le protocole et les clients sont conservés, mais la sémantique serveur est reconstruite autour d’un modèle déclaratif.

---

## 3. Objectifs

### 3.1 Objectifs fonctionnels

Mumble Server Runtime doit permettre :

- une vue Mumble différente pour chaque connexion ;
- la modification dynamique de cette vue sans reconnexion ;
- la fusion et la séparation de groupes logiques ;
- un routage audio indépendant des canaux visibles ;
- l’utilisation du client Mumble officiel ;
- une authentification pilotée par une intégration externe ;
- une autorité serveur complète sur la visibilité, l’audibilité et les interactions ;
- une API d’intégration déclarative ;
- une réconciliation automatique entre vue désirée et état supposé du client ;
- un fonctionnement correct même si un client tente des actions incohérentes ou malveillantes ;
- une implémentation performante sans placer la logique métier dans le chemin audio critique.

### 3.2 Objectifs architecturaux

Le système doit :

- laisser le flavor posséder l'unique source de vérité métier ;
- publier une seule révision de flavor par génération Mumble Server Runtime ;
- rendre les vues de manière déterministe ;
- séparer construction de vue, réconciliation et transport ;
- permettre un rendu complet de référence ;
- autoriser plus tard un rendu incrémental ou retained-mode strictement équivalent ;
- rendre les invariants protocolaires testables ;
- permettre d’expliquer pourquoi une connexion voit ou entend une autre ;
- limiter les dépendances implicites entre intégrations et protocole Mumble.

### 3.3 Objectifs de compatibilité

La première version devrait viser :

- TCP/TLS Mumble ;
- UDP chiffré Mumble ;
- Opus ;
- tunnel audio par TCP en secours ;
- clients Mumble 1.5 ou plus récents ;
- connexion sur une adresse et un port uniques ;
- authentification par jeton opaque transmis dans `Authenticate.password` ;
- arbre de canaux, utilisateurs, permissions effectives et actions contextuelles ;
- audio normal, whisper contrôlé et audio positionnel.

Cette cible devra être confirmée par une matrice de tests sur les clients desktop et mobiles réellement utilisés.

---

## 4. Non-objectifs initiaux

La première version ne cherche pas nécessairement à fournir :

- une compatibilité complète avec Murmur ;
- les anciens codecs CELT ou Speex ;
- les anciens formats UDP si Mumble 1.5+ est imposé ;
- la persistance Murmur ;
- une base SQLite compatible ;
- une API Ice compatible ;
- les serveurs virtuels Murmur ;
- l’annuaire public Mumble ;
- l’éditeur natif complet des ACL ;
- la gestion native de comptes enregistrés ;
- une administration générale depuis le client Mumble ;
- un cluster distribué multi-régions ;
- du mixage audio côté serveur ;
- une protection contre l’enregistrement local ;
- un client Mumble personnalisé.

Les fonctions non supportées doivent être explicitement refusées ou neutralisées, jamais laissées dans un état ambigu.

---

## 5. Principes fondamentaux

### 5.1 L'état métier appartient au flavor

L'intégration choisit librement son modèle métier, sa concurrence et ses
commandes. Mumble Server Runtime reçoit seulement une référence immuable vers le type de
snapshot associé au flavor :

```rust
trait VoiceFlavor: Send + Sync + 'static {
    type Snapshot: Send + Sync + 'static;
}
```

Le runtime ne peut ni inspecter ni modifier ce snapshot sans passer par le
flavor. Il possède séparément l'état vocal nécessaire aux connexions, aux vues
engagées et aux routes audio.

Aucune obligation n’existe d’avoir un objet canonique `Channel` correspondant à chaque canal affiché.

Un canal peut être :

- purement synthétique ;
- différent selon le viewer ;
- un regroupement visuel ;
- une action sélectionnable ;
- une projection d’une équipe ou d’une partie ;
- un conteneur technique destiné à rendre un utilisateur visible.

### 5.2 La vue n’est pas la sécurité

La visibilité d’un utilisateur dans l’interface ne lui donne aucun droit.

```text
Visible(Alice, Bob)
≠
Audible(Alice, Bob)
≠
CanMessage(Alice, Bob)
≠
CanInteract(Alice, Bob)
```

Les graphes doivent être séparés.

### 5.3 Le serveur reste autoritaire

Toute commande du client est une intention :

```text
Commande Mumble
→ résolution dans la vue courante
→ VoiceEvent versionné
→ validation et mutation par le flavor
→ nouveau snapshot métier
→ nouveau rendu
```

Le client ne modifie jamais directement le snapshot métier ou la vue engagée.

### 5.4 Le rendu complet est la spécification

La fonction conceptuelle de référence reste :

```rust
fn render_full<F: VoiceFlavor>(
    flavor: &F,
    viewer: ConnectionId,
    snapshot: &F::Snapshot,
) -> Result<RenderOutput, FlavorError>;
```

Toute optimisation future doit respecter :

```text
render_incremental(previous, changes, snapshot)
==
render_full(snapshot)
```

après normalisation.

### 5.5 Le chemin audio ne rappelle pas les intégrations

Aucun callback distant, plugin ou requête Minecraft ne doit être exécuté pour chaque paquet ou couple émetteur-récepteur.

Le chemin audio doit consulter un snapshot précompilé :

```rust
struct AudioRoutingSnapshot {
    generation: u64,
    receivers_by_sender: Vec<ReceiverSet>,
    source_metadata: Vec<AudioSourceMetadata>,
}
```

---

## 6. Terminologie

### Flavor subject

Référence opaque vers l'identité authentifiée que le flavor associe à une
connexion :

```rust
struct FlavorSubjectRef(OpaqueKey);
```

La structure du principal, ses UUID et ses capacités appartiennent au flavor.
Mumble Server Runtime peut conserver séparément les preuves vocales nécessaires au protocole,
comme le hash du certificat présenté.

### Connection

Connexion réseau Mumble active, comprenant :

- session TCP/TLS ;
- session Mumble ;
- état cryptographique UDP ;
- adresse UDP observée ;
- référence de sujet opaque fournie par le flavor ;
- vue engagée ;
- registre d’interactions ;
- file de sortie ordonnée.

### Flavor snapshot

État métier autoritaire, immuable pendant un rendu et opaque pour Mumble Server Runtime.

### Desired view

Vue que le serveur souhaite actuellement présenter à une connexion.

### Committed view

Vue dont les messages ont été acceptés dans la file de sortie TCP ordonnée.

Ce n’est pas un état explicitement acquitté par le client. Le protocole ne possède pas d’ACK général de vue.

### Projected view

Synonyme conceptuel de la vue spécifique calculée pour une connexion.

### Realm

Exemple de domaine métier défini par un flavor de jeu et regroupant
temporairement ou durablement des joueurs :

- partie ;
- instance ;
- monde ;
- équipe ;
- lobby ;
- groupe vocal.

Un realm n’est pas nécessairement un canal Mumble.

### Audio route

Relation directionnelle autorisant un flux :

```text
sender → receiver
```

La relation peut être asymétrique.

### View key

Identité sémantique stable d’un élément rendu, équivalente à une `key` React.

### View ID

Identifiant numérique ou protocolaire attribué à un élément pour une connexion particulière.

---

## 7. Architecture globale

```text
┌───────────────────────────────┐
│ Application métier            │
│ - état, commandes, acteurs    │
└──────────────┬────────────────┘
               │ snapshot immuable
               ▼
┌───────────────────────────────┐
│ Flavor compilé                │
│ - rendu par connexion         │
│ - politiques audio            │
│ - traduction des événements  │
└───────┬──────────────┬────────┘
        │              │
        │ sorties      └──────────────────────┐
        ▼                                     ▼
┌───────────────────┐              ┌────────────────────┐
│ View Renderer     │              │ Audio Compiler     │
│ par connexion     │              │ global/partitionné │
└─────────┬─────────┘              └──────────┬─────────┘
          ▼                                   ▼
┌───────────────────┐              ┌────────────────────┐
│ Desired View      │              │ Routing Snapshot   │
└─────────┬─────────┘              └──────────┬─────────┘
          ▼                                   ▼
┌───────────────────┐              ┌────────────────────┐
│ Reconciler        │              │ UDP Data Plane     │
└─────────┬─────────┘              └──────────┬─────────┘
          ▼                                   ▼
┌───────────────────┐              ┌────────────────────┐
│ TCP Control Plane │              │ Clients Mumble     │
└───────────────────┘              └────────────────────┘
```

### 7.1 Crates ou modules proposés

```text
mumble-server-runtime-protocol
    framing TCP
    messages Protobuf
    formats UDP
    compatibilité de versions

mumble-server-runtime-crypto
    état OCB2
    nonces
    rejeu
    resynchronisation

voxloom-transport
    TLS
    sockets TCP/UDP
    files de sortie
    association UDP

voxloom-session
    connexions
    sessions
    cycle de vie
    état client

voxloom-auth
    preuves vocales
    certificats
    contexte d'authentification générique
    résultat opaque pour le flavor

voxloom-flavor
    contrat de flavor compilé
    snapshots métier opaques
    sorties de rendu
    événements vocaux

voxloom-render
    composants
    VDOM normalisé
    rendu
    cache
    dépendances

voxloom-reconcile
    diff
    validation
    planification des transitions

voxloom-audio
    compilation du graphe
    routage
    positional audio
    voice targets

voxloom-control
    générations publiées
    coordination vue/audio
    invalidation des connexions
    livraison des événements

voxloom-observe
    métriques
    traces
    inspecteur de vues
    explication des décisions

voxloom-testkit
    client simulé
    tests de propriété
    fuzzing
    scénarios de compatibilité
```

---

## 8. Modèle de rendu déclaratif

Le format réel ne sera pas JSX. Le modèle mental reste néanmoins proche d’un renderer déclaratif.
Les helpers métier de cet exemple appartiennent au flavor et ne font pas partie
de l'API centrale.

### 8.1 API conceptuelle

```rust
fn render_connection(
    ctx: &mut RenderContext,
    viewer: ConnectionId,
) -> ConnectionView {
    let player = ctx.player_for(viewer);
    let match_state = ctx.match_of(player.id);

    view! {
        Channel(key = "connected", name = "Connecté") {
            UserRef(player.session)
        }

        if let Some(game) = match_state {
            MatchView(game = game.id, viewer = player.id)
        }

        AudioPolicy {
            SameInstance
            SameDimension
            Proximity(radius = 40.0)
            SharedRadio
        }
    }
}
```

L’API réelle peut utiliser :

- builders Rust ;
- traits de composants ;
- enums ;
- macros procédurales limitées ;
- fonctions ordinaires retournant des structures immuables.

Le projet ne doit pas réimplémenter React dans Rust pour satisfaire une métaphore. Les concepts utiles sont la pureté, les clés stables, le cache, l’invalidation et la réconciliation.

### 8.2 Sortie de rendu

```rust
struct RenderOutput {
    client_view: ClientView,
    audio_view: ConnectionAudioView,
    handlers: InteractionRegistry,
    effects: Vec<ClientEffect>,
}
```

Les effets ne font pas partie de l’état persistant. Ils doivent être idempotents ou explicitement consommés.

### 8.3 Vue client normalisée

```rust
struct ClientView {
    root_channel: ChannelId,
    channels: BTreeMap<ChannelId, ViewChannel>,
    users: BTreeMap<SessionId, ViewUser>,
    listeners: BTreeSet<ListenerRelation>,
    permissions: BTreeMap<ChannelId, PermissionBits>,
    context_actions: BTreeMap<ActionKey, ContextActionView>,
    server_presentation: ServerPresentation,
}
```

La structure normalisée est préférable à un arbre récursif pur, car Mumble possède des relations transversales :

- liens entre canaux ;
- listeners ;
- voice targets ;
- permissions ;
- actions ciblant canaux ou utilisateurs.

### 8.4 Composants persistants recommandés

#### `ConnectionRoot`

Racine d’une vue spécifique.

```rust
struct ConnectionRoot {
    connection: ConnectionId,
    children: Vec<ViewNode>,
}
```

#### `Channel`

```rust
struct ViewChannel {
    key: ChannelKey,
    id: ChannelId,
    parent: ChannelId,
    name: String,
    description: Option<BlobRef>,
    position: i32,
    temporary: bool,
    max_users: Option<u32>,
    enter_restricted: bool,
    can_enter: bool,
    links: BTreeSet<ChannelId>,
}
```

#### `UserRef`

```rust
struct ViewUser {
    key: UserKey,
    session: SessionId,
    name: String,
    channel: ChannelId,
    user_id: Option<u32>,
    certificate_hash: Option<String>,
    mute: bool,
    deaf: bool,
    suppress: bool,
    self_mute: bool,
    self_deaf: bool,
    priority_speaker: bool,
    recording: bool,
    comment: Option<BlobRef>,
    texture: Option<BlobRef>,
}
```

#### `PermissionSet`

Permissions effectives déjà calculées pour la connexion et le canal.

#### `ContextAction`

Action affichée dans les menus du serveur, d’un canal ou d’un utilisateur.

#### `ListenerView`

Relation d’interface indiquant qu’un utilisateur écoute un canal. Elle n’est pas nécessaire au routage réel.

#### `ServerPresentation`

Paramètres tels que message d’accueil, HTML autorisé, taille maximale de message ou enregistrement annoncé.

### 8.5 Composants non visibles

Ces composants produisent des snapshots internes :

```text
AudioRoute
AudioPolicy
SpatialSource
VoiceTargetPolicy
TextPolicy
PluginDataPolicy
VisibilityPolicy
```

Exemple :

```rust
AudioPolicy::all([
    require_same_instance(),
    deny_different_dimension(),
    allow_proximity(40.0),
    allow_shared_radio(),
])
```

### 8.6 Effets ponctuels

```rust
enum ClientEffect {
    SendText(TextPayload),
    Deny(PermissionDenial),
    Reject(RejectReason),
    Disconnect(DisconnectReason),
    SendPluginData(PluginPayload),
    SuggestConfig(ClientSuggestion),
}
```

Un effet ne doit pas être réémis simplement parce qu’un composant est rerendu.

---

## 9. Identité, clés et allocation d’identifiants

### 9.1 Session IDs

Les sessions utilisateur doivent être :

- uniques dans le processus ou dans le domaine de routage ;
- stables pendant toute la connexion ;
- non réutilisées rapidement ;
- connues du client avant tout paquet audio correspondant.

Recommandation : allocation monotone `u32`, avec détection de wraparound et recyclage uniquement après une période sûre ou redémarrage contrôlé.

### 9.2 Channel IDs

Les Channel IDs peuvent être propres à chaque connexion, mais cela implique :

- une table de résolution par connexion ;
- l’interdiction d’interpréter un ID client comme ID canonique ;
- une stabilité suffisante pour éviter les anomalies de cache client ;
- une absence de réutilisation immédiate.

```rust
struct ViewIdMapping {
    channel_to_view: HashMap<ChannelKey, ChannelId>,
    view_to_channel: HashMap<ChannelId, ChannelKey>,
}
```

### 9.3 Stabilité entre reconnexions

Le client Mumble conserve certaines préférences locales selon le serveur, l’ID de canal, le chemin du canal, la session ou le hash du certificat.

Des IDs arbitraires différents à chaque reconnexion peuvent perturber :

- filtres locaux de canaux ;
- raccourcis ;
- canal de reconnexion désiré ;
- volumes ;
- utilisateurs ignorés ;
- surnoms locaux.

Recommandation :

- attribuer des IDs déterministes aux canaux sémantiquement stables ;
- réserver une plage pour les canaux éphémères ;
- conserver un nom et un hash utilisateur stables ;
- ne pas varier le nom réel d’un utilisateur selon le viewer sans nécessité.

### 9.4 Utilisateurs synthétiques

Un utilisateur synthétique peut représenter :

- une radio ;
- une annonce ;
- un bot ;
- un flux serveur ;
- un utilisateur caché derrière une identité de projection.

Contraintes :

- il doit posséder une session valide et visible ;
- les paquets audio doivent référencer cette session ;
- une même session synthétique ne doit pas transporter simultanément plusieurs flux Opus indépendants sans mixage ;
- les numéros de frame et buffers audio appartiennent conceptuellement à une source.

Un utilisateur synthétique ne doit pas être confondu avec une connexion authentifiée.

### 9.5 Actor IDs

Lorsqu’un message contient un `actor`, cette session doit être visible par le destinataire. Sinon :

- omettre le champ ;
- utiliser une entité synthétique visible ;
- envoyer un message serveur sans acteur.

---

## 10. Authentification

Les jetons, principals et associations propres à Minecraft décrits ci-dessous
appartiennent au flavor Minecraft. Mumble Server Runtime fournit seulement le contexte vocal
et les preuves protocolaires nécessaires à leur validation.

### 10.1 Endpoint unique

Un seul endpoint suffit :

```text
voice.example.net:64738
```

La vue n’est pas choisie par le port. Elle est calculée après authentification
par le flavor à partir de son snapshot métier et de l'identité vocale résolue.

### 10.2 Jeton à usage unique

Flux recommandé :

```text
1. Connexion Minecraft.
2. Génération d’un jeton aléatoire, court dans le temps et à usage unique.
3. Le joueur ouvre Mumble.
4. Le jeton est transmis dans le champ password.
5. Mumble Server Runtime consomme atomiquement le jeton.
6. Le jeton est résolu vers un UUID Minecraft.
7. Le certificat Mumble peut être mémorisé comme association durable.
8. La vue initiale est rendue.
```

Propriétés du jeton :

- au moins 128 bits d’entropie réelle ;
- expiration courte ;
- consommation atomique ;
- association facultative à l’IP comme contrainte secondaire ;
- invalidation après succès ;
- limitation des tentatives.

### 10.3 Certificat client

Après association :

```text
certificate hash ↔ principal
```

Le certificat peut permettre les reconnexions sans nouveau jeton, selon la politique de sécurité.

Questions à définir :

- un principal peut-il posséder plusieurs certificats ?
- un certificat peut-il être transféré ?
- comment révoquer une association ?
- le jeton est-il requis à chaque session de jeu ?
- le certificat peut-il reconnecter lorsque le joueur n’est pas connecté à Minecraft ?

### 10.4 IP et ports comme identité

L’IP ne doit pas être considérée comme une preuve suffisante :

- NAT partagé ;
- VPN ;
- IPv6 temporaire ;
- changement de route ;
- plusieurs joueurs sur le même réseau ;
- différence de chemin entre Minecraft et Mumble.

Un port temporaire choisi dans une plage peut servir de signal d’amorçage, mais ne remplace pas le jeton.

L’association recommandée est :

```text
jeton = preuve
certificat = identité durable
IP/port = indice ou restriction secondaire
```

### 10.5 Nom d’utilisateur

Le `username` reçu du client est une proposition d’affichage, pas une identité autoritaire. Le serveur doit pouvoir :

- l’ignorer ;
- le normaliser ;
- le remplacer par le nom Minecraft canonique ;
- le refuser si invalide.

---

## 11. Cycle de vie d’une connexion

### 11.1 Transport TCP

Le canal de contrôle utilise TLS et un framing :

```text
2 octets : type de message, big-endian
4 octets : longueur, big-endian
N octets : payload
```

La plupart des payloads sont des messages Protobuf.

Particularité importante : le type `UDPTunnel` utilise historiquement le type TCP correspondant, mais transporte les octets audio UDP directement. Il ne faut pas le sérialiser naïvement comme le message Protobuf `UDPTunnel` déclaré mais inutilisé.

### 11.2 Séquence initiale proposée

```text
Client → Version
Client → Authenticate

Serveur → Version
Serveur → CryptSetup
Serveur → ChannelState(root)
Serveur → ChannelState(canaux initiaux, parents avant enfants)
Serveur → UserState(self)
Serveur → UserState(utilisateurs visibles)
Serveur → PermissionQuery éventuels
Serveur → ContextActionModify éventuels
Serveur → ServerSync
Serveur → ServerConfig
Serveur → CodecVersion(opus = true), si requis
```

Invariants :

- l’utilisateur local doit être connu avant `ServerSync` ;
- tous les canaux référencés doivent exister ;
- les parents doivent précéder les enfants ;
- les utilisateurs doivent référencer un canal visible ;
- le root channel utilise l’ID `0`.

### 11.3 Établissement UDP

Le serveur :

- génère la clé et les nonces ;
- transmet `CryptSetup` ;
- accepte les pings UDP ;
- associe l’adresse UDP à la connexion après authentification cryptographique réussie ;
- permet une mise à jour d’adresse en cas de NAT rebinding, sur preuve cryptographique ;
- resynchronise les nonces lorsque nécessaire.

L’adresse IP seule ne doit jamais sélectionner la session cryptographique.

### 11.4 Déconnexion

À la fermeture TCP :

- retirer la connexion de l'état vocal du runtime ;
- émettre l'événement de déconnexion vers le flavor ;
- invalider les routes audio ;
- détruire l’état cryptographique ;
- libérer la vue engagée ;
- rerendre toutes les connexions dans la première version ;
- ne pas conserver un `CommittedView` réutilisable pour une future connexion.

---

## 12. Réconciliation

### 12.1 États comparés

```rust
struct ConnectionRenderState {
    committed: Arc<ClientView>,
    desired: Arc<ClientView>,
    revision: u64,
}
```

`committed` représente l’état dont la séquence de messages a été acceptée dans la file de sortie ordonnée.

### 12.2 Étapes

```text
render
→ normalize
→ validate
→ diff
→ build dependency graph
→ order operations
→ enqueue transaction
→ commit shadow state
```

### 12.3 Normalisation

La normalisation doit :

- garantir le root channel ;
- trier les collections ;
- supprimer les valeurs par défaut inutiles ;
- résoudre les clés vers IDs ;
- vérifier l’absence de cycles ;
- vérifier toutes les références ;
- garantir la présence du self-user ;
- calculer les propriétés dérivées ;
- produire une représentation déterministe.

### 12.4 Diff logique

```rust
struct ViewDelta {
    channels_added: Vec<ViewChannel>,
    channels_updated: Vec<ChannelPatch>,
    channels_removed: Vec<ChannelId>,

    users_added: Vec<ViewUser>,
    users_updated: Vec<UserPatch>,
    users_removed: Vec<SessionId>,

    permissions_updated: Vec<PermissionUpdate>,
    actions_added: Vec<ContextActionView>,
    actions_removed: Vec<ActionKey>,
    listener_updates: Vec<ListenerUpdate>,
}
```

### 12.5 Planification protocolaire

Le diff ne doit pas être envoyé dans un ordre arbitraire.

#### Ajout ou déplacement

```text
1. Créer les nouveaux canaux parents.
2. Créer les canaux enfants.
3. Mettre à jour les propriétés de canaux.
4. Ajouter les nouveaux utilisateurs.
5. Déplacer les utilisateurs vers les nouveaux canaux.
6. Mettre à jour leurs états.
7. Publier permissions et actions.
8. Activer les nouvelles routes audio.
```

#### Restriction ou suppression

```text
1. Désactiver immédiatement les routes audio devenues interdites.
2. Invalider les voice targets ou handlers concernés.
3. Retirer ou déplacer les utilisateurs.
4. Retirer les listeners.
5. Supprimer les canaux, enfants avant parents.
6. Retirer les actions contextuelles obsolètes.
```

### 12.6 Sécurité des transitions

Pour une nouvelle autorisation :

```text
vue d’abord
audio ensuite
```

Pour une nouvelle interdiction :

```text
audio d’abord
vue ensuite
```

Cela évite de livrer brièvement un flux interdit.

### 12.7 Transaction de sortie

```rust
struct OutputTransaction {
    from_revision: u64,
    to_revision: u64,
    messages: Vec<OutboundMessage>,
    next_view: Arc<ClientView>,
}
```

La vue engagée n’est mise à jour que si la transaction entière est acceptée par la file de sortie.

Si l’écriture réseau échoue ensuite, la connexion doit être considérée perdue. Il n’existe pas de mécanisme général permettant de connaître précisément le dernier message appliqué par le client.

### 12.8 Resynchronisation

Stratégies possibles :

1. **reconnexion forcée**, simple et sûre ;
2. **reconstruction complète contrôlée**, plus complexe ;
3. ajout futur d’un mécanisme interne de génération uniquement pour les intégrations, sans prétendre que le client acquitte cette génération.

La version initiale devrait préférer la reconnexion lorsqu’une divergence grave est détectée.

---

## 13. Rendu complet, cache et rendu incrémental

### 13.1 Version initiale

```text
publication d'un snapshot de flavor
→ sélectionner `All` dans la première version
→ rerendre entièrement chaque connexion
→ réconcilier
```

Cette version sert de référence.

### 13.2 Cache de sous-arbres immuables

```rust
struct ComponentCache {
    props_fingerprint: u64,
    dependencies: Vec<(DependencyKey, Revision)>,
    output: Arc<VNode>,
}
```

Si les props et dépendances sont inchangées :

```rust
return cache.output.clone();
```

Le réconciliateur peut ignorer un sous-arbre partagé :

```rust
if Arc::ptr_eq(old, new) {
    return;
}
```

### 13.3 Invalidation

```rust
enum DirtyState {
    Clean,
    DescendantDirty,
    SelfDirty,
}
```

Une modification marque :

- le composant dépendant comme `SelfDirty` ;
- ses ancêtres comme `DescendantDirty` ;
- les branches indépendantes restent `Clean`.

### 13.4 Suivi des dépendances

La première version rerend toutes les connexions pour chaque publication. Une
future invalidation ciblée utilisera des clés opaques définies par le flavor et
gardera `All` comme fallback correct. Elle relève de la phase d'optimisation et
ne fait pas partie du contrat P7 initial.

### 13.5 Retained-mode futur

Un composant coûteux peut conserver un objet de vue :

```rust
trait RetainedProjection<S> {
    fn rebuild(&self, snapshot: &S) -> Arc<VNode>;

    fn try_update(
        &self,
        previous: &Arc<VNode>,
        change: &FlavorChange,
        snapshot: &S,
    ) -> Option<Arc<VNode>>;
}
```

En cas inconnu :

```rust
let next = try_update(...).unwrap_or_else(|| rebuild(state));
```

La vue retained reste un cache reconstructible, jamais une source de vérité.

### 13.6 Vérification de l’équivalence

```rust
#[test]
fn incremental_render_matches_full_render() {
    for change in generated_changes() {
        state.apply(change);

        let incremental = runtime.render_incremental();
        let reference = runtime.render_full();

        assert_eq!(
            normalize(incremental),
            normalize(reference),
        );
    }
}
```

Un mode de développement peut comparer aléatoirement une fraction des rendus incrémentaux au rendu complet.

### 13.7 Positions hors du VDOM

Les positions ne doivent pas dirtifier le renderer structurel à chaque tick.

```text
position update
→ index spatial
→ recomputation locale des routes
→ publication AudioRoutingSnapshot
```

Le VDOM gère les changements de structure, pas chaque déplacement.

---

## 14. Vue visible et routage audio

### 14.1 Indépendance fondamentale

```text
Channel tree
≠
Audio graph
```

Le client joue un paquet audio si :

- il est correctement reçu et déchiffré ;
- son codec est supporté ;
- `sender_session` correspond à un utilisateur connu ;
- les filtres locaux ne le bloquent pas.

Le client ne recalcule pas les ACL ou l’appartenance de canal pour décider si le paquet était légitime.

### 14.2 Entendre un utilisateur situé dans un autre canal

Valide :

```text
Alice affichée dans canal A
Bob affiché dans canal B
route Bob → Alice autorisée
```

Le serveur transmet le paquet de Bob à Alice.

### 14.3 Même canal sans audibilité

Valide :

```text
Alice et Bob affichés dans le même canal
route Bob → Alice absente
```

Alice ne reçoit rien.

### 14.4 Utilisateur audible mais caché

Le client officiel doit connaître une session pour attribuer et bufferiser le flux.

Solutions :

- rendre l’utilisateur dans un canal synthétique ;
- rendre une identité synthétique par source ;
- mixer réellement plusieurs sources en un flux serveur ;
- utiliser un client personnalisé.

La première solution est recommandée.

### 14.5 Contextes audio

Le serveur peut annoter les paquets sortants comme :

- normal ;
- shout ;
- whisper ;
- listen.

Ces contextes influencent l’interface et certains filtres locaux. Ils ne doivent pas être traités comme une preuve de politique.

### 14.6 Volume adjustment

Le serveur peut joindre un ajustement de volume, mais le client reste libre de l’appliquer.

Une politique de sécurité ne doit jamais reposer sur un volume nul. Pour interdire l’écoute, ne pas transmettre le paquet.

### 14.7 Positionnel

Les coordonnées envoyées sont une représentation virtuelle en trois dimensions.

La conversion Minecraft recommandée est configurable :

```text
1 bloc = 1 unité Mumble
```

ou une conversion explicite en mètres si nécessaire.

Le serveur doit distinguer :

- autorisation de recevoir le paquet ;
- inclusion des coordonnées positionnelles ;
- position utilisée ;
- atténuation locale du client.

---

## 15. Data plane audio

### 15.1 Pipeline entrant

```text
datagramme UDP
→ association cryptographique
→ déchiffrement
→ anti-rejeu
→ décodage d’enveloppe
→ validation de la session émettrice
→ validation du target
→ rate limit
→ consultation du snapshot de routage
→ émission vers les destinataires
```

### 15.2 Pas de décodage Opus par défaut

Pour router :

- conserver le payload Opus ;
- réécrire les métadonnées nécessaires ;
- encoder l’enveloppe sortante ;
- chiffrer séparément pour chaque destinataire.

Décoder Opus n’est nécessaire que pour :

- mixer plusieurs sources ;
- appliquer un DSP côté serveur ;
- transcoder ;
- analyser le contenu audio.

### 15.3 Snapshot de routage

```rust
struct AudioRoutingSnapshot {
    generation: u64,
    routes: Arc<RouteMatrix>,
    senders: Arc<SenderMetadata>,
}
```

Représentations possibles :

- bitsets ;
- listes compactes de destinataires ;
- groupes partagés ;
- index spatial ;
- combinaison d’ensembles précalculés.

### 15.4 Politique directionnelle

```rust
fn may_receive(
    snapshot: &AudioRoutingSnapshot,
    sender: SessionId,
    receiver: SessionId,
    target: AudioTarget,
) -> AudioDecision;
```

```rust
struct AudioDecision {
    deliver: bool,
    include_position: bool,
    context: AudioContext,
    volume_adjustment: Option<f32>,
}
```

### 15.5 Targets

Le target `0` correspond à la parole normale. Les targets personnalisés sont enregistrés par `VoiceTarget`. Le target réservé au loopback serveur doit être traité explicitement.

Mumble Server Runtime peut :

- supporter les targets standard ;
- traduire les targets vers sa propre politique ;
- limiter le nombre ou les formes ;
- refuser les références non visibles ;
- ignorer les groupes ACL traditionnels.

### 15.6 UDP et tunnel TCP

Le serveur doit supporter le fallback audio via TCP pour les environnements où UDP échoue.

Le tunnel utilise le framing TCP de type `UDPTunnel` mais transporte le paquet audio brut dans le payload.

### 15.7 Limites

Prévoir :

- taille maximale stricte des datagrammes ;
- limite de débit par connexion ;
- limite de paquets ;
- protection contre les targets invalides ;
- rejet des codecs non supportés ;
- métriques de pertes, retards et resynchronisations.

---

## 16. Interactions entrantes du client

Chaque message est converti vers une commande typée, puis validé.

### 16.1 `Version`

Usage :

- négociation de compatibilité ;
- observabilité ;
- choix du format UDP ;
- refus d’un client trop ancien.

### 16.2 `Authenticate`

Entrées :

- username ;
- password ;
- tokens ;
- support Opus ;
- type de client.

Mumble Server Runtime traite le mot de passe comme un credential opaque.

### 16.3 `Ping`

Le serveur répond avec le timestamp et ses statistiques.

### 16.4 `CryptSetup`

Permet :

- initialisation ;
- demande de resynchronisation ;
- mise à jour de nonce.

### 16.5 `UserState`

Intentions possibles :

- demander un déplacement ;
- self-mute ;
- self-deaf ;
- annoncer l’enregistrement ;
- modifier le contexte positionnel ;
- ajouter ou retirer un listener ;
- changer des access tokens temporaires.

Tout champ visant un autre utilisateur doit être refusé sauf capability explicite.

### 16.6 `ChannelState`

Peut être une tentative de :

- créer un canal ;
- renommer ;
- déplacer ;
- modifier la description ;
- ajouter des liens.

Version initiale recommandée : refuser sauf intégration explicitement prévue.

### 16.7 `ChannelRemove`

Tentative de suppression de canal. Refus par défaut.

### 16.8 `UserRemove`

Tentative de kick ou ban. Refus par défaut hors capability administrative.

### 16.9 `TextMessage`

Résoudre les sessions et Channel IDs dans la vue de l’émetteur, puis :

- valider les destinataires ;
- filtrer le contenu ;
- appliquer les limites ;
- rerouter selon la politique ;
- ne jamais laisser le client cibler un utilisateur caché par un ID deviné.

### 16.10 `BanList`

Peut être une requête ou une tentative de remplacement. Répondre vide ou refuser si l’administration native n’est pas supportée.

### 16.11 `ACL`

Peut être :

- une requête pour l’éditeur ;
- une tentative de mise à jour.

Les ACL détaillées sont optionnelles. Les permissions effectives peuvent être générées directement sans reproduire les règles Murmur.

### 16.12 `QueryUsers`

Peut résoudre noms et IDs enregistrés. Attention aux fuites d’utilisateurs invisibles.

### 16.13 `ContextAction`

Résoudre l'action dans le registre actuel de la connexion, revérifier les
conditions vocales et émettre un `VoiceEvent` versionné. Le flavor revérifie
ensuite sa politique métier.

### 16.14 `UserList`

Requête administrative relative aux utilisateurs enregistrés. Optionnelle.

### 16.15 `VoiceTarget`

Enregistre ou supprime une cible locale à la connexion.

Validation :

- ID autorisé ;
- sessions visibles ;
- canaux visibles ;
- pas de référence à une autre vue ;
- limites de taille ;
- groupes ACL éventuellement interdits.

### 16.16 `PermissionQuery`

Le client demande les permissions effectives d’un canal visible.

Le serveur répond à partir de la sortie validée du flavor pour la génération
courante, pas d'un cache non fiable.

### 16.17 `UserStats`

Le client peut demander des informations détaillées sur un utilisateur. Ne pas exposer :

- adresse IP ;
- certificats ;
- version ;
- temps de connexion ;
- statistiques ;

sans autorisation et visibilité explicites.

### 16.18 `RequestBlob`

Le client peut demander :

- texture utilisateur ;
- commentaire utilisateur ;
- description de canal.

Vérifier que la cible existe dans sa vue actuelle.

### 16.19 `PluginDataTransmission`

Valider :

- sender réel ;
- destinataires visibles et autorisés ;
- taille ;
- `dataID` autorisé ;
- rate limit.

### 16.20 Audio UDP/TCP

Valider :

- codec ;
- session ;
- target ;
- état mute/suppress ;
- débit ;
- taille ;
- séquence ;
- autorisation de parole.

---

## 17. Interactions sortantes du serveur

### Connexion et configuration

- `Version`
- `Reject`
- `ServerSync`
- `CryptSetup`
- `Ping`
- `CodecVersion`
- `ServerConfig`
- `SuggestConfig`
- fermeture de connexion

### Vue des canaux

- `ChannelState`
- `ChannelRemove`
- `PermissionQuery`
- `ACL`, uniquement si supportée

### Vue des utilisateurs

- `UserState`
- `UserRemove`
- `UserStats`
- `QueryUsers`
- `UserList`

### Communication

- `TextMessage`
- `PermissionDenied`
- `ContextActionModify`
- `PluginDataTransmission`

### Audio

- audio UDP ;
- audio tunnelé par TCP ;
- contexte normal, whisper, shout ou listen ;
- position ;
- volume advisory.

---

## 18. Autorité serveur et rollback

### 18.1 Actions réversibles

Pour une demande de déplacement :

```text
Client demande canal B.
Serveur valide.
Si accepté : VoiceEvent émis vers le flavor.
Le flavor publie éventuellement un nouveau snapshot.
Mumble Server Runtime rend puis publie UserState.
Si refusé : PermissionDenied, snapshot inchangé.
```

Le serveur peut republier un état autoritaire si nécessaire.

### 18.2 Actions irréversibles

Une fois livré, le serveur ne peut pas reprendre :

- un paquet audio ;
- un message texte ;
- une donnée plugin ;
- une notification ;
- un blob ;
- une information sensible.

Ces opérations doivent être autorisées avant émission.

### 18.3 Pas de mutation optimiste du shadow state

Le `CommittedView` ne change jamais sur simple réception d’une demande client.

### 18.4 Client modifié

Un client peut :

- envoyer des commandes non proposées par l’UI ;
- deviner des IDs ;
- mentir sur son état ;
- ignorer les suggestions ;
- enregistrer localement.

Toutes les validations sont donc serveur-side.

---

## 19. Permissions et ACL

### 19.1 Permissions effectives

Le flavor calcule directement un masque par canal et connexion :

```rust
fn permissions_for<S, P>(
    principal: &P,
    channel: ChannelKey,
    snapshot: &S,
) -> PermissionBits;
```

Les permissions incluent notamment :

- Write ;
- Traverse ;
- Enter ;
- Speak ;
- MuteDeafen ;
- Move ;
- MakeChannel ;
- LinkChannel ;
- Whisper ;
- TextMessage ;
- MakeTempChannel ;
- Listen ;
- Kick ;
- Ban ;
- Register ;
- SelfRegister ;
- ResetUserContent.

### 19.2 Indications de canal

`is_enter_restricted` et `can_enter` servent à l’interface.

Ils ne remplacent pas la validation d’une commande d’entrée.

### 19.3 ACL détaillées

Options :

1. **non supportées**, réponse de refus ;
2. **lecture seule synthétique**, risquant de donner une fausse représentation ;
3. **adaptateur administratif dédié**, futur ;
4. **implémentation Murmur complète**, non recommandée sans besoin réel.

Décision recommandée en v1 : permissions effectives supportées, éditeur ACL non supporté.

### 19.4 Invalidation du cache

Utiliser `PermissionQuery.flush` lorsqu’un grand ensemble de permissions devient obsolète.

---

## 20. Invariants protocolaires

Le validateur doit garantir au minimum :

1. Le canal racine `0` existe.
2. Le root n’est jamais supprimé.
3. Tout canal enfant possède un parent visible.
4. L’arbre de parentage ne contient aucun cycle.
5. Tout utilisateur visible appartient à un canal visible.
6. Le self-user existe avant `ServerSync`.
7. Toute session audio sortante correspond à un utilisateur connu.
8. Un canal occupé n’est jamais supprimé.
9. Les canaux sont créés parents avant enfants.
10. Les canaux sont supprimés enfants avant parents.
11. Les IDs sont uniques dans la vue.
12. Les IDs retirés ne sont pas réutilisés immédiatement.
13. Toute action entrante est résolue dans la vue de l’émetteur.
14. Toute session ou tout canal ciblé par le client est visible dans sa vue.
15. Les actors invisibles ne sont pas référencés.
16. Les blobs ne sont servis que pour des entités visibles.
17. Les informations d’administration ne révèlent pas d’entités cachées.
18. Les routes audio interdites sont supprimées avant la transition visuelle.
19. Les routes nouvellement autorisées ne sont activées qu’après préparation de la vue.
20. Le shadow state n’est engagé qu’après acceptation atomique du plan de sortie.

---

## 21. Particularités du client Mumble à anticiper

### 21.1 `UserRemove` ressemble à une déconnexion

Utiliser `UserRemove` pour chaque changement de proximité provoquerait :

- notifications ;
- logs ;
- TTS ;
- churn visuel.

La proximité doit modifier le routage, pas la présence visible.

### 21.2 Préférences locales par certificat

Le hash de certificat sert au client pour :

- amis ;
- mute local ;
- ignore ;
- volume ;
- surnom.

Conserver un hash et une identité stables.

### 21.3 Caches locaux de canaux

Le client peut mémoriser des propriétés selon le serveur et le Channel ID.

Éviter de réutiliser le même ID pour des significations différentes, surtout entre reconnexions.

### 21.4 Raccourcis et targets

Les raccourcis locaux peuvent dépendre :

- du serveur ;
- du canal ;
- du chemin ;
- de l’utilisateur.

Les vues hautement éphémères peuvent rendre ces raccourcis incohérents. Documenter clairement le niveau de support.

### 21.5 Noms variables par viewer

Possible, mais les préférences locales restent liées au hash. Cela peut créer une expérience étrange où un utilisateur renommé différemment conserve un surnom ou un volume historique.

Recommandation : identité stable, présentation contextuelle portée par les canaux ou commentaires.

### 21.6 Channel links

Même si les liens ne sont pas utilisés pour router, le client les représente. Ne pas les publier sans sémantique utile.

### 21.7 Listeners

Les listeners visibles ne sont pas requis pour entendre un canal. Si leur interface est exposée, leurs commandes doivent être traduites ou refusées proprement.

### 21.8 Recording allowed

La configuration peut désactiver la fonction intégrée du client, mais pas empêcher un enregistrement externe.

### 21.9 SuggestConfig

Ce ne sont que des suggestions. Ne pas en faire une frontière de sécurité.

---

## 22. Sécurité

### 22.1 Frontières de confiance

Non fiables :

- client Mumble ;
- username ;
- état self-mute ;
- position fournie par plugin ;
- plugin identity ;
- contexte positionnel ;
- targets ;
- IDs fournis par le client ;
- IP comme identité unique.

Fiables selon le déploiement :

- état Minecraft serveur ;
- service d’authentification ;
- snapshot métier publié par le flavor via un canal approuvé ;
- snapshots signés ou transport interne authentifié.

### 22.2 Position autoritaire

La position utilisée pour l’autorisation doit venir du serveur Minecraft.

La position client peut servir à fluidifier le rendu spatial, mais pas à autoriser la réception.

### 22.3 Fail closed

En cas de :

- état expiré ;
- principal non associé ;
- realm inconnu ;
- snapshot incohérent ;
- erreur de résolution ;

aucun audio sensible n’est transmis.

### 22.4 Fuites de métadonnées

Vérifier tous les chemins :

- audio ;
- UserState ;
- UserStats ;
- TextMessage ;
- QueryUsers ;
- UserList ;
- ACL ;
- BanList ;
- PluginData ;
- RequestBlob ;
- ContextAction ;
- VoiceTarget ;
- métriques administratives.

Cacher un utilisateur dans l’arbre ne suffit pas si son adresse IP apparaît dans `UserStats`.

### 22.5 Rate limits

Appliquer des limites à :

- authentification ;
- messages TCP ;
- taille de payload ;
- audio ;
- changements de target ;
- text messages ;
- plugin data ;
- demandes de blobs ;
- requêtes de statistiques ;
- actions contextuelles.

### 22.6 Crypto

L’implémentation OCB2 doit être :

- testée contre le client officiel ;
- couverte par des vecteurs de test ;
- isolée dans un module ;
- fuzzée ;
- protégée contre rejeu et nonce reuse ;
- auditée avant exposition publique.

---

## 23. Concurrence

### 23.1 Control plane

L'application et son flavor possèdent la sérialisation des mutations métier :

- acteurs, locks ou transactions métier ;
- commandes et événements applicatifs ;
- batching et révisions du snapshot métier.

Mumble Server Runtime reçoit une révision immuable déjà publiée. Son coordinateur sérialise
uniquement la génération vocale correspondante : rendus, transitions de vues,
routes audio et événements `VoiceEvent`. Il ne tient jamais un
`RwLock<F::Snapshot>` pendant les rendus.

### 23.2 Data plane

Le chemin audio peut être multi-threadé et consulter :

```rust
Arc<AudioRoutingSnapshot>
```

publié par swap atomique.

### 23.3 Cohérence entre vue et audio

Une publication de flavor produit :

```rust
struct PublishedGeneration {
    generation: u64,
    flavor_revision: FlavorRevision,
    views: ViewGeneration,
    audio: AudioRoutingSnapshot,
}
```

L’ordre de publication respecte les règles de sécurité.

### 23.4 Batching

Le flavor peut coalescer les changements d'un même cycle avant publication :

```text
changement de partie
+ changement d’équipe
+ changement de rôle
→ un seul rendu final
```

Une petite fenêtre événementielle ou une transaction explicite appartient à
l'intégration. Mumble Server Runtime ne voit que le snapshot final et ne rend aucun état
intermédiaire.

---

## 24. API d’intégration

### 24.1 Modèle par flavors et snapshots

L'intégration compile un flavor avec Mumble Server Runtime. Le flavor possède son snapshot et
ses commandes. Le runtime ne connaît pas leur structure :

```rust
trait VoiceFlavor: Send + Sync + 'static {
    type Snapshot: Send + Sync + 'static;

    fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision;

    fn render(
        &self,
        snapshot: &Self::Snapshot,
        connection: ConnectionId,
    ) -> Result<RenderOutput, FlavorError>;
}
```

`RenderOutput` porte une `DesiredClientView` basée sur des clés sémantiques, des
routes entre `ConnectionId` et un `InteractionRegistry`. Chaque route est
déclarée par sa connexion receveuse et n'est valide que si sa vue projette
l'émetteur. Les `ChannelId` et `SessionId` numériques sont attribués ensuite par
Mumble Server Runtime et ne traversent jamais la frontière du flavor.

Une publication P7 fournit uniquement un `Arc<F::Snapshot>` ; le runtime rend
toutes les connexions. Une éventuelle sélection ciblée reste une optimisation
P10 avec fallback `All`.

```rust
struct RenderedSnapshot<S> {
    snapshot: Arc<S>,
    flavor_revision: FlavorRevision,
    outputs: BTreeMap<ConnectionId, RenderOutput>,
}

struct ValidatedSnapshot<S> {
    rendered: RenderedSnapshot<S>,
}
```

La validation vérifie séparément toutes les vues désirées, toutes les routes
audio et tous les registres d'interactions. Une erreur rejette la génération
entière. `RenderedSnapshot` et `ValidatedSnapshot` restent sans effet ; seule la
publication atomique de la tranche suivante peut modifier l'état vocal engagé.

Le runtime émet vers le flavor :

```rust
enum VoiceEvent {
    Connected { ... },
    AuthenticationFailed { ... },
    ChannelInteractionRequested { ... },
    ContextActionInvoked { ... },
    TextSubmitted { ... },
    VoiceTargetChanged { ... },
    Disconnected { ... },
}
```

### 24.2 Intégration embarquée

Un binaire de composition choisit un `VoiceFlavor` à la compilation. La
dépendance va du flavor vers l'API Mumble Server Runtime ; aucune crate centrale ne dépend
d'un flavor concret. Le flavor traite les `VoiceEvent`, modifie son propre état
selon son modèle de concurrence, puis publie un nouveau snapshot si nécessaire.

Cette intégration statique n'impose ni ABI de plugin dynamique ni callbacks
stockés dans le hot path.

### 24.3 Intégration externe

Une intégration externe future peut envelopper un flavor et accepter :

- snapshots métier complets versionnés ;
- patches métier idempotents ;
- commandes propres au flavor ;
- abonnements aux événements.

Le protocole de cette API appartient au flavor. Il ne doit jamais être appelé
depuis le hot path audio.

### 24.4 Handlers stables

Éviter de stocker des closures nouvellement allouées dans le VDOM.

```rust
struct HandlerRef {
    component: ComponentInstanceId,
    action: ActionKey,
    generation: u64,
}
```

À l’invocation :

- vérifier que le handler existe encore ;
- vérifier la génération ;
- revérifier les conditions vocales ;
- laisser le flavor revérifier sa politique métier après réception de
  l'événement.

---

## 25. Observabilité

### 25.1 Métriques

Connexion :

- connexions actives ;
- authentifications réussies/refusées ;
- temps de handshake ;
- reconnexions ;
- erreurs TLS.

Rendu :

- nombre de connexions invalidées ;
- durée de rendu ;
- taille des vues ;
- nombre d’opérations de diff ;
- cache hit ratio ;
- fallback vers rendu complet ;
- plans rejetés par validation.

Audio :

- paquets entrants/sortants ;
- destinataires moyens ;
- pertes ;
- paquets tardifs ;
- resync crypto ;
- débit ;
- temps dans le routeur ;
- routes refusées.

Sécurité :

- commandes invalides ;
- IDs hors vue ;
- targets interdits ;
- rate limits ;
- fuites empêchées.

### 25.2 Inspecteur de vue

API administrative :

```text
GET /debug/connections/{id}/view
GET /debug/connections/{id}/committed-view
GET /debug/connections/{id}/desired-view
GET /debug/connections/{id}/diff
```

### 25.3 Explication des décisions

```rust
fn explain_audio(
    sender: SessionId,
    receiver: SessionId,
) -> AudioDecisionExplanation;
```

Exemple :

```text
DENY
- same_instance: true
- same_dimension: false
- shared_radio: false
- proximity: 18.2m / 40m
- final rule: different_dimension deny
```

### 25.4 Replay

Journaliser les publications de flavor et les `VoiceEvent` déterministes
permettent de :

- reproduire un bug ;
- rejouer une séquence ;
- comparer rendu incrémental et complet ;
- vérifier un incident de routage.

---

## 26. Tests

### 26.1 Tests unitaires de protocole

- framing TCP ;
- parsing partiel ;
- limites de taille ;
- sérialisation Protobuf ;
- tunnel audio TCP ;
- format UDP ;
- crypto ;
- resynchronisation.

### 26.2 Golden tests avec client officiel

Scénarios :

- connexion ;
- initial sync ;
- ajout/retrait d’utilisateur ;
- création/déplacement/suppression de canal ;
- permission flush ;
- context actions ;
- audio UDP ;
- fallback TCP ;
- NAT rebinding ;
- reconnect.

### 26.3 Tests de propriété du réconciliateur

Générer deux vues valides :

```text
old
new
```

Puis vérifier que :

- le plan n’enfreint aucun invariant intermédiaire ;
- l’application du plan au client simulé produit `new` ;
- aucun canal occupé n’est supprimé ;
- aucune référence inconnue n’apparaît.

### 26.4 Tests de rendu

```text
incremental == full
retained == full
cached == uncached
```

### 26.5 Fuzzing

Cibles :

- framing ;
- Protobuf ;
- paquets UDP ;
- crypto ;
- commandes client ;
- diff ;
- normalisation ;
- planificateur ;
- résolution d’IDs.

### 26.6 Client simulé

Le testkit doit implémenter un modèle strict du client :

```rust
struct SimulatedMumbleClient {
    channels: HashMap<ChannelId, Channel>,
    users: HashMap<SessionId, User>,
    permissions: HashMap<ChannelId, PermissionBits>,
}
```

Il rejette les violations comme le client officiel.

### 26.7 Tests de confidentialité

Vérifier qu’un utilisateur caché ne fuit jamais via :

- audio ;
- presence ;
- texte ;
- stats ;
- blobs ;
- plugin data ;
- targets ;
- acteurs ;
- listes administratives.

---

## 27. Performance

### 27.1 Priorités

1. correction ;
2. hot path audio précompilé ;
3. rerender complet de toutes les connexions comme oracle ;
4. cache immuable ;
5. structural sharing ;
6. dépendances automatiques ;
7. retained-mode spécialisé.

### 27.2 Complexité cible

Rendu structurel après une éventuelle invalidation ciblée mesurée :

```text
O(nombre d’éléments visibles pour les connexions affectées)
```

Diff :

```text
O(changements + branches comparées)
```

Audio :

```text
O(nombre de destinataires autorisés)
```

La sélection des destinataires ne doit pas être `O(nombre total de connexions)` par paquet lorsque l’échelle augmente.

### 27.3 Précomputation

Utiliser :

- bitsets pour groupes denses ;
- petites listes compactes pour groupes faibles ;
- partage de groupes ;
- index spatial ;
- cache par realm ou équipe ;
- snapshots immuables.

### 27.4 Profiling obligatoire

Ne pas implémenter le retained-mode avant d’avoir mesuré :

- temps de rendu ;
- taux de changements ;
- taille moyenne des vues ;
- coût du diff ;
- pression mémoire ;
- coût dominant réel.

Le rendu complet reste probablement insignifiant pour quelques centaines de joueurs et des changements structurels occasionnels.

---

## 28. Déploiement

### 28.1 Première architecture

Un processus unique :

```text
TCP/TLS acceptor
UDP socket
Flavor integration
Mumble Server Runtime runtime
View renderer
Reconciler
Audio router
Control API
```

Avantages :

- cohérence simple ;
- sessions locales ;
- crypto locale ;
- pas de migration distribuée ;
- debugging réaliste.

### 28.2 Multi-instance futur

Architecture possible :

```text
Mumble Edge
├── TLS/TCP sessions
├── UDP crypto
├── session IDs
└── client views

Control Cluster
├── flavor state service
└── render decisions

Audio Workers
├── routing partitions
└── cross-worker transport
```

Difficultés :

- ownership de session ;
- migration ;
- stable session IDs ;
- réplication ;
- ordre des vues ;
- transport inter-worker ;
- reprise après panne ;
- cohérence entre crypto et routage.

Ne pas commencer par cette architecture.

### 28.3 NixOS

Le service devrait exposer :

- configuration déclarative ;
- certificat TLS ;
- endpoint TCP/UDP ;
- secrets d’authentification ;
- métriques ;
- journal structuré ;
- limites de ressources ;
- utilisateur système dédié.

---

## 29. Roadmap

### Phase 0 : exploration protocolaire

Livrables :

- connexion TLS ;
- framing TCP ;
- décodage `Version` et `Authenticate` ;
- client officiel connecté ;
- documentation des écarts desktop/mobile.

Critère : le client atteint une session minimale sans Murmur.

### Phase 1 : serveur vocal minimal

- root channel ;
- self user ;
- `ServerSync` ;
- crypto UDP ;
- ping ;
- Opus ;
- loopback ;
- deux clients capables de s’entendre ;
- fallback TCP.

Critère : appel vocal stable avec client officiel.

### Phase 2 : projection par connexion

- `ClientView` ;
- canaux synthétiques ;
- utilisateurs visibles par connexion ;
- ID mapping ;
- normalisation ;
- full render ;
- diff ;
- transition planner.

Critère : Alice et Bob voient des arbres différents sans reconnexion.

### Phase 3 : intégration de flavor

- contrat `VoiceFlavor` ;
- snapshot métier opaque ;
- rendu complet de toutes les connexions ;
- publication atomique des vues et routes ;
- événements vocaux versionnés ;
- flavor de référence et binaire de composition.

Critère : le scénario Aurora/Borealis de la Phase 2 passe uniquement à travers
l'API publique de flavor.

### Phase 4 : flavor Minecraft

- jetons ;
- association UUID ;
- état joueur ;
- realms ;
- équipes ;
- dimensions ;
- positions autoritaires ;
- proximité ;
- changements sans reconnexion.

Critère : plusieurs parties isolées sur un endpoint unique.

### Phase 5 : interactions

- permissions effectives ;
- context actions ;
- texte contrôlé ;
- voice targets ;
- radios ;
- listeners traduits ou refusés ;
- inspecteur et explications.

### Phase 6 : robustesse

- property tests ;
- fuzzing ;
- compatibilité mobile ;
- rate limits ;
- replay ;
- métriques ;
- tests longue durée.

### Phase 7 : optimisation

- invalidation ciblée ;
- cache de sous-arbres ;
- structural sharing ;
- dependency tracking ;
- retained-mode sur profils coûteux.

### Phase 8 : distribution éventuelle

Uniquement si l’échelle réelle l’exige.

---

## 30. Décisions proposées pour la v1

| Sujet                     | Décision proposée                               |
| ------------------------- | ----------------------------------------------- |
| Nom                       | Mumble Server Runtime                           |
| Client minimum            | Mumble 1.5+                                     |
| Codec                     | Opus uniquement                                 |
| Endpoint                  | Un port TCP/UDP commun                          |
| Authentification          | Jeton à usage unique, puis certificat optionnel |
| IP comme identité         | Non                                             |
| Serveurs virtuels         | Non                                             |
| Canaux                    | Projection UI                                   |
| Routage                   | Graphe indépendant                              |
| ACL Murmur                | Non reproduites                                 |
| Permissions effectives    | Oui                                             |
| Éditeur ACL               | Refusé                                          |
| Text chat                 | Support limité et autoritaire                   |
| Voice targets             | Sous-ensemble validé                            |
| Listeners                 | Traduits ou refusés, jamais autoritaires        |
| Utilisateurs synthétiques | Support encadré                                 |
| Rendu                     | Full render par connexion affectée              |
| Cache                     | Plus tard                                       |
| Hot path                  | Snapshot précompilé                             |
| Cluster                   | Hors v1                                         |
| Opus decode               | Non                                             |
| Position sécurité         | Minecraft autoritaire                           |
| Divergence grave          | Reconnexion                                     |

---

## 31. Questions ouvertes

### Protocole et clients

1. Mumble 1.5+ suffit-il pour tous les clients cibles ?
2. Faut-il supporter l’ancien format UDP ?
3. Quels clients mobiles doivent être certifiés ?
4. Le loopback serveur est-il indispensable dès la v1 ?
5. Quel comportement adopter face aux champs inconnus ou versions futures ?

### Identité

6. Le certificat permet-il une reconnexion permanente ?
7. Une session de jeu active est-elle obligatoire ?
8. Comment gérer plusieurs clients Mumble pour un même UUID ?
9. Comment révoquer un certificat ?
10. Le username Mumble est-il totalement ignoré ?

### Vue

11. Les Channel IDs doivent-ils être déterministes entre reconnexions ?
12. Un utilisateur peut-il avoir un nom différent selon le viewer ?
13. Quels éléments restent toujours visibles ?
14. Les utilisateurs hors portée restent-ils visibles dans une partie ?
15. Faut-il un canal technique « Participants » ?

### Audio

16. Comment combiner proximité et radio lorsqu’elles autorisent simultanément un flux ?
17. Quel contexte audio afficher pour chaque route ?
18. Faut-il appliquer une position différente selon le mode radio ?
19. Comment gérer plusieurs radios ?
20. Quel modèle d’hystérésis et d’expiration des positions utiliser ?

### Interactions

21. Le chat texte est-il nécessaire ?
22. Les déplacements manuels de canal ont-ils une signification métier ?
23. Les listeners sont-ils visibles ?
24. Les raccourcis whisper natifs doivent-ils fonctionner ?
25. Quelles context actions sont exposées ?

### Administration

26. Existe-t-il une UI externe dédiée ?
27. Les bans sont-ils gérés par Minecraft, Mumble Server Runtime ou les deux ?
28. Faut-il exposer les statistiques réseau au client ?
29. Les comptes enregistrés Mumble sont-ils totalement supprimés ?
30. Quel niveau de compatibilité avec les outils d’administration existants ?

### Exploitation

31. Quelle durée de replay conserver ?
32. Quelles données doivent être anonymisées ?
33. Quel budget CPU/mémoire par connexion ?
34. Quelle taille maximale de vue ?
35. Quelle politique de surcharge et de backpressure ?

---

## 32. Risques principaux

### Risque 1 : compatibilité implicite du client

Le protocole documenté ne décrit pas toutes les attentes historiques du client. Des tests réels restent indispensables.

### Risque 2 : crypto UDP

Une erreur de nonce, rejeu ou resynchronisation peut rendre le serveur instable ou vulnérable.

### Risque 3 : fuite entre vues

Une entité cachée peut fuir par un chemin secondaire non couvert par le renderer principal.

### Risque 4 : IDs instables

Des IDs réutilisés ou changeants peuvent casser raccourcis, filtres et préférences locales.

### Risque 5 : confusion vue/politique

Une intégration pourrait supposer qu’un utilisateur visible est audible. Les types doivent empêcher cette confusion.

### Risque 6 : optimisation prématurée

Un retained-mode trop tôt transformerait la vue en seconde source de vérité.

### Risque 7 : callbacks dans le hot path

Une API trop flexible pourrait permettre des callbacks par paquet, détruisant latence et fiabilité.

### Risque 8 : sémantique Mumble accidentellement réintroduite

Si les composants `Channel` deviennent la source du routage, le projet recréera Murmur sous un nom plus à la mode.

---

## 33. Critères d’acceptation du MVP

Le MVP est réussi lorsque :

1. deux clients Mumble officiels se connectent au même endpoint ;
2. ils sont authentifiés par jeton ;
3. chaque client reçoit une vue différente ;
4. les vues changent sans reconnexion ;
5. les canaux synthétiques n’ont pas besoin d’exister dans le snapshot métier ;
6. le routage audio ne dépend pas des canaux visibles ;
7. un utilisateur inconnu n’est jamais utilisé comme source audio ;
8. les transitions ne provoquent aucune violation protocolaire ;
9. les actions client sont validées serveur-side ;
10. le rendu complet et le client simulé passent les property tests ;
11. aucune fuite entre deux realms n’est détectée ;
12. un inspecteur peut expliquer chaque décision d’audibilité ;
13. une perte UDP déclenche le fallback TCP ;
14. les nonces peuvent être resynchronisés ;
15. le serveur tient une charge réaliste sans décoder Opus.

---

## 34. Exemple complet de modèle

Cet exemple vit dans le flavor Minecraft de la Phase 4. Il n'appartient pas aux
crates centrales de Mumble Server Runtime.

```rust
fn render_voice_world(
    ctx: &mut RenderContext,
    connection: ConnectionId,
) -> RenderOutput {
    let viewer = ctx.viewer(connection);
    let player = ctx.player(viewer.player_id);
    let game = ctx.game_of(player.id);

    let mut view = ClientViewBuilder::new(connection);

    let connected = view.channel(
        ChannelKey::Static("connected"),
        ChannelProps {
            name: "Connecté".into(),
            ..Default::default()
        },
    );

    view.user(player.session)
        .name(player.name.clone())
        .channel(connected);

    if let Some(game) = game {
        let match_channel = view.channel(
            ChannelKey::Match(game.id),
            ChannelProps {
                name: game.display_name.clone(),
                ..Default::default()
            },
        );

        for other in game.visible_players_for(player.id) {
            view.user(other.session)
                .name(other.name.clone())
                .channel(match_channel);
        }
    }

    let audio = ctx.audio_builder()
        .deny_if(different_instance())
        .deny_if(different_dimension())
        .allow_if(within_distance(40.0))
        .allow_if(shared_radio())
        .compile_for(connection);

    let handlers = InteractionRegistry::builder()
        .context_action(
            ActionKey::Static("invite-party"),
            ActionTarget::User,
            CommandTemplate::InviteParty,
        )
        .build();

    RenderOutput {
        client_view: view.build(),
        audio_view: audio,
        handlers,
        effects: Vec::new(),
    }
}
```

Le renderer ne connaît pas :

- l’ordre des messages Mumble ;
- le framing TCP ;
- la crypto UDP ;
- les règles de suppression de canal ;
- les détails de `PermissionDenied`.

Le planificateur prend en charge ces contraintes.

---

## 35. Conclusion

Mumble Server Runtime doit être conçu autour de quatre abstractions indépendantes :

```text
FlavorSnapshot (opaque)
ProjectedClientView
AudioRoutingSnapshot
InteractionRegistry
```

Le pipeline de contrôle est :

```text
snapshot publié par le flavor
→ rendu du flavor
→ validation Mumble Server Runtime
→ réconciliation
→ messages Mumble
```

Le pipeline audio est :

```text
paquet
→ validation
→ snapshot de routage
→ destinataires
→ chiffrement
→ envoi
```

La valeur principale du projet ne réside pas dans le fait de réécrire Murmur en Rust. Elle réside dans le remplacement de sa sémantique partagée par un runtime déclaratif où chaque connexion reçoit une réalité cohérente, calculée et indépendante.

Le protocole Mumble devient une cible de rendu et un transport audio. Les canaux deviennent une interface. Les permissions deviennent des indications calculées. Le graphe d’audibilité devient la vraie topologie vocale.

C’est cette séparation qui rend possible un serveur vocal dynamique, pilotable et raisonnable à tester, plutôt qu’une collection de conditions dispersées qui finirait inévitablement par autoriser un fantôme mort dans une autre dimension à écouter la radio d’une équipe adverse.

---

# Annexes

## Annexe A. Matrice complète des messages TCP

Le protocole courant définit 27 types de messages TCP. Le tableau suivant fixe une proposition de comportement pour Mumble Server Runtime v1.

| Message                  | Direction habituelle           |                      Support v1 proposé | Comportement                                                             |
| ------------------------ | ------------------------------ | --------------------------------------: | ------------------------------------------------------------------------ |
| `Version`                | bidirectionnel                 |                                  Requis | Négociation et observabilité.                                            |
| `UDPTunnel`              | bidirectionnel                 |                                  Requis | Payload audio UDP brut transporté dans le framing TCP.                   |
| `Authenticate`           | client → serveur               |                                  Requis | Résolution du jeton et du principal.                                     |
| `Ping`                   | bidirectionnel                 |                                  Requis | Keepalive et statistiques.                                               |
| `Reject`                 | serveur → client               |                                  Requis | Refus explicite de connexion.                                            |
| `ServerSync`             | serveur → client               |                                  Requis | Termine la synchronisation initiale.                                     |
| `ChannelRemove`          | bidirectionnel                 |  Requis côté serveur, refus côté client | Retrait de vue ; demandes client refusées par défaut.                    |
| `ChannelState`           | bidirectionnel                 | Requis côté serveur, limité côté client | Projection de canaux ; création/modification client refusée par défaut.  |
| `UserRemove`             | bidirectionnel                 | Requis côté serveur, limité côté client | Disparition de vue ; kick/ban soumis à capability.                       |
| `UserState`              | bidirectionnel                 |                                  Requis | Présence, déplacement demandé, self-state, listeners et contexte plugin. |
| `BanList`                | bidirectionnel                 |                               Optionnel | Réponse vide ou refus si administration native absente.                  |
| `TextMessage`            | bidirectionnel                 |                                  Limité | Routage autoritaire, limites et résolution dans la vue.                  |
| `PermissionDenied`       | serveur → client               |                                  Requis | Refus de commandes.                                                      |
| `ACL`                    | bidirectionnel                 |                               Non en v1 | Éditeur natif refusé ; permissions effectives séparées.                  |
| `QueryUsers`             | bidirectionnel                 |                                  Limité | Seulement pour entités visibles et autorisées.                           |
| `CryptSetup`             | bidirectionnel                 |                                  Requis | Clés, nonces et resynchronisation.                                       |
| `ContextActionModify`    | serveur → client               |                                  Requis | Ajout/retrait d’actions déclaratives.                                    |
| `ContextAction`          | client → serveur               |                                  Requis | Dispatch vers un handler actuel et revalidation.                         |
| `UserList`               | serveur → client après requête |                               Non en v1 | Refus ou liste vide.                                                     |
| `VoiceTarget`            | client → serveur               |                                  Limité | Targets validés, sessions et canaux visibles uniquement.                 |
| `PermissionQuery`        | bidirectionnel                 |                                  Requis | Permissions effectives et invalidation de cache.                         |
| `CodecVersion`           | serveur → client               |                 Requis ou compatibilité | Annonce Opus uniquement.                                                 |
| `UserStats`              | bidirectionnel                 |                                  Limité | Statistiques minimales, sans fuite d’IP ou certificat.                   |
| `RequestBlob`            | client → serveur               |                Requis si blobs utilisés | Réponse seulement pour les entités visibles.                             |
| `ServerConfig`           | serveur → client               |                                  Requis | Configuration d’interface et limites.                                    |
| `SuggestConfig`          | serveur → client               |                               Optionnel | Suggestions non autoritaires.                                            |
| `PluginDataTransmission` | bidirectionnel                 |                     Optionnel et filtré | Allowlist de `dataID`, visibilité et rate limits.                        |

Politique générale pour une fonction non supportée :

```text
commande valide mais non supportée
→ PermissionDenied avec raison claire

message malformé ou abusif
→ compteur d’erreur
→ éventuel disconnect après seuil
```

Le serveur ne doit pas silencieusement accepter une commande qu’il n’applique pas, car le client pourrait alors conserver une attente incorrecte.

## Annexe B. Matrice des messages UDP

| Message | Direction      | Support | Notes                                                                                             |
| ------- | -------------- | ------: | ------------------------------------------------------------------------------------------------- |
| `Audio` | bidirectionnel |  Requis | Opus, target entrant, context sortant, session source, frame number, position et volume advisory. |
| `Ping`  | bidirectionnel |  Requis | Détection UDP, RTT et informations serveur optionnelles.                                          |

Le serveur doit également connaître les anciens formats de paquets suffisamment pour :

- les refuser proprement ;
- ou les supporter si la cible Mumble 1.5+ n’est finalement pas imposée.

## Annexe C. ADR initiales

### ADR-001 : l'état métier est indépendant de Mumble et de Mumble Server Runtime

**Décision :** les canaux, ACL et serveurs virtuels Murmur ne sont pas le modèle
métier. Ce modèle appartient au flavor et reste opaque pour le runtime, selon
`docs/decisions/0002-flavor-owns-business-state.md`.

**Conséquence :** toutes les références client sont résolues vers des
`VoiceEvent` ou des clés de flavor avant de quitter le runtime vocal.

### ADR-002 : le routage audio est indépendant de l’arbre visible

**Décision :** aucune règle implicite « même canal = audible » n’existe dans le cœur.

**Conséquence :** les composants visuels et les politiques audio produisent des sorties distinctes.

### ADR-003 : full render comme oracle de correction

**Décision :** toute vue doit être reconstructible intégralement par le flavor
depuis un snapshot métier immuable.

**Conséquence :** les caches et projections retained peuvent être supprimés sans changer le comportement.

### ADR-004 : validation avant effet

**Décision :** toute action client est validée avant émission d'un `VoiceEvent`
ou effet irréversible. Le flavor valide séparément toute mutation métier.

**Conséquence :** les corrections rétroactives restent un mécanisme de récupération, pas le flux normal.

### ADR-005 : pas de callback métier dans le hot path audio

**Décision :** le routeur consulte uniquement un snapshot local.

**Conséquence :** les politiques doivent être compilées lors des changements d’état.

### ADR-006 : endpoint unique

**Décision :** un même couple adresse/port accueille toutes les connexions.

**Conséquence :** l’authentification et le principal déterminent la vue, pas le port.

### ADR-007 : IDs de canal locaux mais stables

**Décision :** un Channel ID peut être spécifique à une connexion, mais doit rester stable pour une même clé de vue.

**Conséquence :** toutes les commandes entrantes nécessitent une résolution par connexion.

### ADR-008 : permissions effectives sans ACL Murmur

**Décision :** le client reçoit les permissions calculées, mais l’éditeur ACL n’est pas supporté en v1.

**Conséquence :** la politique réelle peut provenir de Minecraft ou d’un moteur arbitraire.

### ADR-009 : reconnexion sur divergence grave

**Décision :** aucune tentative complexe de rollback partiel après corruption du shadow state.

**Conséquence :** les invariants et la transaction de sortie doivent rendre ce cas exceptionnel.

### ADR-010 : processus unique avant distribution

**Décision :** l’architecture initiale termine TLS, UDP et contrôle dans le même runtime.

**Conséquence :** la fédération de workers ne doit pas déformer les abstractions v1.

## Annexe D. Sources protocolaires de référence

La spécification doit être vérifiée en continu contre les sources officielles du projet Mumble, notamment :

- `src/Mumble.proto` pour les messages TCP ;
- `src/MumbleUDP.proto` pour les messages audio et ping UDP ;
- `src/MumbleProtocol.h` et `src/MumbleProtocol.cpp` pour les types, contextes et encodages ;
- `src/Connection.cpp` pour le framing TCP ;
- `src/mumble/ServerHandler.cpp` pour la réception audio côté client ;
- `src/mumble/Messages.cpp` pour l’application des messages à la vue client ;
- `src/ACL.h` pour les permissions ;
- les tests protocolaires officiels lorsqu’ils existent.

Le comportement du client officiel fait partie de la compatibilité réelle. Les fichiers `.proto` seuls ne constituent pas une spécification comportementale complète.
