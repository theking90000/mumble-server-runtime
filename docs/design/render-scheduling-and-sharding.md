# Modèle de rendu, d'ordonnancement et de sharding (proposition)

> **Document historique.** Il conserve le diagnostic et le raisonnement qui ont
> mené au runtime à shards. La carte de crates proposée au §13 était une étape
> intermédiaire et n'a pas été retenue telle quelle. L'implémentation courante
> est `mumble-server-runtime-shard` + `mumble-server-runtime-gateway`, décrite intégralement dans
> `guide-implementation.md`, qui fait foi.
>
> **Statut : RAISONNEMENT ARCHIVÉ — implémentation achevée ailleurs.**
> Dernière révision : 2026-07-27 (révision 6). Voir le journal (§15).
>
> ⚠️ **Dépassé sur le modèle de visibilité.** Les sections qui parlent de `Params`
> (masque, réécritures, overlay) décrivent une étape intermédiaire du
> raisonnement. Le modèle courant — **portées en arbre, overlay privé et relation
> audio orientée, séparés en trois mécanismes** — est décrit dans
> `guide-implementation.md`, qui fait foi. Ce document reste utile pour le
> _pourquoi_ : diagnostic du quadratique, ordonnancement, sharding, migration,
> plan UDP, découpage en crates.

---

## 1. Pourquoi

Le pipeline P5–P7 était correct et vérifié, mais son coût était quadratique
et son ordonnancement est implicite. Les deux défauts ont **la même cause
unique**, et c'est une signature :

```rust
fn render(&self, snapshot: &Snapshot, connection: ConnectionId) -> RenderOutput
```

L'unité de rendu est _le monde entier vu par une connexion_. Tout le reste en
découle mécaniquement :

- N connexions, chacune voyant une vue de taille O(N) ⇒ matérialiser toutes les
  vues coûte Θ(N²). Aucun ordonnanceur, aucun cache ne corrige cela : c'est dans
  le type.
- Les vues étant par connexion, les IDs le sont aussi, donc les frames aussi,
  donc **la sérialisation est refaite N fois** pour un contenu identique.
- Chaque transaction étant indépendante, il faut un protocole de commit par
  connexion (`PendingTransition`, `CommitToken`, `PublicationCommit`, epochs),
  puis un coordinateur pour en séquencer N, puis un verrou autour du
  coordinateur.

La machinerie de commit, le coordinateur, le mutex global et les quatre sites
d'appel dispersés de `publish_generation()` ne sont pas la maladie : ce sont les
symptômes obligés d'un rendu par connexion.

**Mesures existantes** (`ci/bench-publication.sh`, checklist P7 signée) :

| connexions | publication complète | par connexion |
| ---------- | -------------------- | ------------- |
| 2          | 25,75 µs             | 12,88 µs      |
| 10         | 140,58 µs            | 14,06 µs      |
| 50         | 1,35 ms              | 26,90 µs      |
| 200        | 21,01 ms             | 105,04 µs     |
| 500        | **198,21 ms**        | 396,42 µs     |

Le coût _par connexion_ passe de 13 µs à 396 µs : la vue de chaque connexion
grossit avec N, parce qu'elle contient les autres connexions.

Amplification supplémentaire, indépendante : `connection.rs:200` déclenche une
génération complète après **chaque** frame de contrôle drainée par n'importe
quelle connexion. Un tick produisant N frames planifie 1 publication utile + N
publications à vide, chacune en O(N).

### 1.1 Notations

| symbole | sens                                                                                                                   |
| ------- | ---------------------------------------------------------------------------------------------------------------------- |
| `N`     | nombre de connexions                                                                                                   |
| `W`     | taille d'un shard : nombre d'éléments **distincts** (canaux, utilisateurs, relations), chaque fait compté **une fois** |
| `V`     | taille d'**une** vue (`W` filtré pour un spectateur)                                                                   |
| `D`     | taille d'un delta (ce qui a changé entre deux versions du shard)                                                       |
| `S_c`   | masque de visibilité de la connexion `c` : le sous-ensemble de `W` qu'elle voit                                        |

Point clé : `V = O(N)` parce qu'une vue contient des utilisateurs. `W = O(N)`
parce que chaque utilisateur est **un** fait, quel que soit le nombre de gens qui
le voient. `N × V` compte des copies ; `W` compte des faits.

---

## 2. L'idée centrale

> **On réconcilie un shard une fois par tick. Chaque connexion attachée reçoit ce
> delta partagé, filtré et retouché, plus trois termes qui valent zéro tant que
> rien n'a changé _pour elle_.**

L'intuition à retenir : on ne partage pas les **vues**, on partage les
**changements**. C'est une propriété beaucoup plus robuste — elle tient même
quand toutes les vues sont différentes, parce qu'un delta de trois opérations
filtré N fois coûte O(N·|D|) et non O(N·V).

Quatre concepts, et rien d'autre :

| concept                           | rôle                                                                                                                                          |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| **Shard**                         | objet runtime de première classe : unité de **possession**, d'**ordonnancement** (une task), de rendu, d'allocation d'IDs et de routage audio |
| **Version + journal de deltas**   | l'état engagé du shard, versionné, et les deltas déjà encodés en octets                                                                       |
| **Cursor** (`u64`, par connexion) | jusqu'où cette connexion a été avancée dans le journal                                                                                        |
| **Params** (par connexion)        | le « fine-tune » : masque de visibilité, réécritures, overlay privé                                                                           |

L'état filaire d'une connexion n'est jamais matérialisé. Il est **dérivé** :

```
C_wire(c) = f_{params_c}( Shard[cursor_c] ) ⊕ private_c
```

---

## 3. Le Shard

### 3.1 Ce qu'il possède

Un Shard possède, et est le seul à posséder :

- son **état métier** (fourni par le flavor, voir §4) ;
- son **contenu rendu** : la `ShardView` courante, sa version, son journal de
  deltas encodés ;
- l'**état de contrôle de chaque connexion attachée** : `cursor`, `params`,
  émetteur de file de sortie ;
- son **domaine d'allocation d'IDs** (bail par blocs, §10.4) ;
- son **domaine de routage audio** : la voix ne franchit jamais une frontière de
  shard.

**Aucune référence croisée entre shards.** Deux shards ne partagent aucun état,
donc ils peuvent être deux tasks, deux processus ou deux machines sans protocole
de coordination.

### 3.2 Cycle de vie

Le Shard n'est **pas** dérivé d'une énumération que le runtime demanderait au
flavor. Il est créé et détruit **impérativement**, à tout moment, sans qu'un
événement de connexion soit nécessaire :

```rust
let (shard, handle) = runtime.create_shard(MinecraftGame::new(game_id))?;
runtime.move_connection(conn, shard);
handle.send(GameEvent::RoundStarted);   // message métier typé
runtime.wake(shard);                    // forcer un re-rendu
runtime.destroy_shard(shard, "match terminé");
```

Détruire un shard qui possède encore des connexions n'est pas une erreur : le
runtime les détache d'abord (§10.3). Où elles vont ensuite est une décision du
flavor — soit il les a déplacées avant, soit la politique de repli du
`ConnectionRouter` s'applique, soit elles sont fermées.

### 3.3 Une task par shard

```
┌── task Shard A ──────────────┐   ┌── task Shard B ──────────────┐
│  état métier                 │   │  état métier                 │
│  ShardView + journal         │   │  ShardView + journal         │
│  {conn → cursor, params}     │   │  {conn → cursor, params}     │
│  ArcSwap<ShardRouting>  ─────┼─┐ │  ArcSwap<ShardRouting>       │
└──────────────┬───────────────┘ │ └──────────────┬───────────────┘
     push octets (non bloquant)  │      push octets (non bloquant)
               ▼                 │                ▼
   ┌── task conn 1 ──┐  ┌── task conn 2 ──┐   ┌── task conn 3 ──┐
   │ socket TLS      │  │ socket TLS      │   │ socket TLS      │
   │ crypto OCB2     │  │ crypto OCB2     │   │ crypto OCB2     │
   └─────────────────┘  └─────────────────┘   └─────────────────┘
                                 │
                    ┌── task plan UDP ───┴───────────┐
                    │ lit les ShardRouting répliqués │
                    └────────────────────────────────┘
```

**La task de shard n'attend jamais d'I/O.** Sa boucle :

```rust
loop {
    dirty.notified().await;        // wake() du flavor, ou un événement runtime
    drain_commands();              // observe() reste immédiat
    drain_until(next_publication).await; // immédiat si le plafond est déjà passé
    reconcile();                   // une publication pour toute la fenêtre
    next_publication = Instant::now() + MIN_INTERVAL;
}
```

**Il n'y a aucun tick dans le runtime.** Mumble Server Runtime ne sait pas _pourquoi_ un flavor
voudrait être re-rendu périodiquement, donc il ne l'impose ni ne le propose : il
se contente de réagir à un réveil et de le plafonner (§4.3).

Elle ne fait que du CPU et des opérations de canal non bloquantes. Les écritures
socket appartiennent aux tasks de connexion, qui drainent des files bornées.
Conséquences :

- **Parallélisme naturel** : K shards sur un runtime tokio multi-thread ⇒ K
  cœurs. Les shards étant indépendants, la mise à l'échelle est linéaire.
- **Isolation de panne** : une task de shard qui panique ne tue que son shard.
  Le runtime observe le `JoinHandle`, ferme les connexions attachées et loggue.
  C'est un **bénéfice** du modèle, pas une conséquence subie.
- **Contrat pour le flavor** : dans une task de shard, il est interdit de
  bloquer, de faire de l'I/O synchrone ou un calcul long. Le travail lourd se
  fait ailleurs et arrive par message.

---

## 4. Le contrat de flavor

Le flavor éclate en **deux rôles**, parce qu'ils répondent à deux questions
différentes et vivent à deux endroits différents.

### 4.1 `ConnectionRouter` — où va une connexion qui arrive ?

Une connexion qui vient de s'authentifier n'appartient à aucun shard : personne
ne peut donc décider _depuis_ un shard. Le routeur est un objet de niveau
runtime, fourni par le binaire de composition.

```rust
pub trait ConnectionRouter: Send + Sync + 'static {
    /// Appelé une fois, sur la task de la connexion, jamais sur une task de
    /// shard. Peut donc awaiter : valider un jeton, interroger un service.
    async fn route(&self, identity: &ConnectionIdentity) -> RouteDecision;
}

pub struct ConnectionIdentity {
    pub name: String,
    pub certificate_hash: Option<String>,
    /// Le credential opaque du champ `Authenticate.password`.
    pub credential: Option<String>,
}

pub enum RouteDecision {
    Attach(ShardId),
    Reject(String),
}
```

**C'est ici qu'atterrit le flux de jeton de P8 (spec 10.2).** Le routeur consomme
atomiquement le jeton, le résout vers un principal Minecraft et rend le shard de
sa partie. Le runtime n'apprend rien du métier : il reçoit un `ShardId` ou un
refus. Le seul autre endroit qui touchait au credential (`connection.rs`, qui le
jette aujourd'hui) disparaît.

### 4.2 `ShardLogic` — trois méthodes, pas une de plus

Mumble Server Runtime pose exactement trois questions à un shard : _à quoi ressembles-tu_,
_comment cette connexion est-elle retouchée_, et _voici ce qui s'est passé_. Tout
le reste quitte le domaine de Mumble Server Runtime pour entrer dans le métier.

```rust
pub trait ShardLogic: Send + 'static {
    /// Le contenu partagé du shard. Aucune notion de spectateur ici : ce qui
    /// est rendu ne peut pas dépendre de qui regarde.
    fn render(&mut self) -> ShardView;

    /// Le fine-tune d'une connexion attachée.
    fn params(&mut self, connection: ConnectionId) -> Params;

    /// Fait vocal observé dans ce shard. Le seul des trois qui peut agir.
    fn observe(&mut self, event: &VoiceEvent);
}
```

**Pas de `tick()`.** Mumble Server Runtime ne sait pas pourquoi un flavor voudrait un rythme.
Un flavor qui en veut un le fabrique lui-même (§4.3).

**Pas de `on_message()`, pas de type `Message`.** Mumble Server Runtime ne sait pas ce qu'un
message métier signifie, donc il ne fournit pas de boîte aux lettres : **le flavor
possède la sienne**. C'est ce qui résout la question « alors `ShardLogic` devrait
être partagé ? » — non :

```rust
struct MinecraftGame {
    handle: ShardHandle,
    inbox: mpsc::Receiver<GameEvent>,   // le flavor possède son canal
    world: World,
}

impl ShardLogic for MinecraftGame {
    fn render(&mut self) -> ShardView {
        // Drainer sa propre boîte, puis rendre. `&mut self` : aucun verrou.
        while let Ok(event) = self.inbox.try_recv() {
            self.world.apply(event);
        }
        self.world.to_view()
    }
    // …
}
```

L'intégration extérieure tient l'émetteur, pousse ses messages, puis appelle
`handle.wake()`.

La borne est **`Send + 'static`, et surtout pas `Sync`** — au sens où Mumble Server Runtime ne
le _demande pas_. Un flavor concret peut se trouver `Sync` (il le sera dès qu'il
contient un `Arc<Mutex<…>>`), c'est son affaire. Ce que dit la borne, c'est que
**le runtime n'a jamais besoin de partager la logique**, donc il ne force aucune
synchronisation interne, et `&mut self` permet de mémoïser sans `Mutex` ni
`RefCell`. Où vivent les `Arc` et les `Mutex` quand il en faut : §4.9.

### 4.3 `wake()` : la seule interface métier → runtime

C'est _toute_ la question, et elle mérite une réponse nette.

Mumble Server Runtime **ne peut pas savoir** quand l'état d'un flavor a changé : cet état est
opaque et souvent extérieur au processus. Il n'a donc que deux options : sonder
(rendre à chaque tick et diffuser, ce qui brûle du CPU à vide et impose un rythme
arbitraire), ou être **prévenu**. `wake()` est ce « prévenu », et il ne transporte
aucune donnée :

> **`wake()` = « mon état a changé, re-rends-moi quand tu peux ».**

Conséquences, et c'est là que le design se simplifie :

- **Le rythme appartient au flavor, la protection au runtime.** Une horloge à
  10 Hz dans un nom de canal ? L'intégration lance son propre
  `tokio::interval(100ms)` et appelle `wake()`. Des positions à 20 Hz ? Pareil.
  Mumble Server Runtime, lui, garantit seulement qu'il ne re-rendra pas plus d'une fois par
  `MIN_INTERVAL` — un flavor qui réveille à 10 kHz ne peut pas le noyer.
- **Le runtime n'a plus aucun tick.** Rien dans Mumble Server Runtime n'a besoin d'un rythme
  propre : les budgets de voix sont pilotés par l'arrivée des paquets, les
  timeouts vivent dans les tasks de connexion, et le routage audio se recompile à
  partir du rendu.

### 4.4 Une seule poignée : `ShardHandle`

`ShardControl` emprunté pendant `observe` était une complication inutile :
l'extérieur a besoin des mêmes opérations, et il n'est pas dans `observe`. Une
seule poignée, clonable, `Send + Sync`, remise à la construction :

```rust
/// Lié à UN shard. Le flavor le stocke ; l'intégration en clone autant qu'elle
/// veut, depuis n'importe quel thread.
impl ShardHandle {
    pub fn wake(&self);
    pub fn move_connection(&self, connection: ConnectionId, to: ShardId);
    pub fn close_connection(&self, connection: ConnectionId, reason: &str);
    pub fn runtime(&self) -> &RuntimeHandle;
}

/// Global. L'œuf et la poule sont résolus par une closure : la logique reçoit
/// sa poignée à la construction.
impl RuntimeHandle {
    pub fn create_shard<L: ShardLogic>(
        &self,
        build: impl FnOnce(ShardHandle) -> L,
    ) -> ShardHandle;
    pub fn destroy_shard(&self, shard: ShardId, reason: &str);
    pub fn move_connection(&self, connection: ConnectionId, to: ShardId);
}
```

```rust
let game = runtime.create_shard(|h| MinecraftGame::new(game_id, h, rx));
```

`observe` n'a donc pas besoin de paramètre supplémentaire : le flavor tient déjà
sa poignée. Et le même objet sert _dedans_ (réagir à un événement vocal) et
_dehors_ (l'intégration décide de déplacer un joueur). Un vocabulaire, pas deux.

### 4.5 Ce que ça rend possible gratuitement : le shard piloté par RPC

Preuve que trois méthodes suffisent — un shard dont la logique est pilotée à
distance ne demande **rien** à Mumble Server Runtime :

```rust
struct RpcShard {
    handle: ShardHandle,
    desired: Arc<Mutex<DesiredState>>,   // écrit par le serveur gRPC
}

impl ShardLogic for RpcShard {
    fn render(&mut self) -> ShardView { self.desired.lock().to_view() }
    fn params(&mut self, c: ConnectionId) -> Params { self.desired.lock().params(c) }
    fn observe(&mut self, event: &VoiceEvent) { /* → gRPC sortant, ou rien */ }
}

// Handler gRPC, sur son propre thread :
//     desired.lock().apply(request);
//     handle.wake();
```

Mumble Server Runtime ne connaît ni HTTP, ni gRPC, ni la forme du `DesiredState`. La variante
« logique figée pilotée par des commandes prédéfinies » est le même code avec un
`DesiredState` plus contraint. Seule contrainte, qui est celle de tout flavor :
**les sections critiques doivent rester courtes**, parce qu'elles s'exécutent dans
la task du shard.

### 4.6 Vue opérationnelle : ce que fait un serveur qui tourne

**Démarrage** (binaire de composition) :

1. construire le `Runtime` (sockets TCP/UDP, allocateurs d'IDs, table de
   bindings) ;
2. installer le `ConnectionRouter` ;
3. créer les shards initiaux, ou aucun si le routeur les crée à la demande ;
4. `serve()`.

**Arrivée d'une connexion** : TLS → `Version` → `Authenticate` →
`router.route(identity).await` (c'est là que le jeton P8 est consommé) →
`Attach(shard)` ou `Reject`. La task de connexion existe ; le shard n'apprend son
existence qu'à l'attachement, par un `VoiceEvent::Connected`.

**En régime** : les tasks de shard dorment sur leur `Notify`. Un `wake()` de
l'intégration ou un événement vocal en réveille une ; elle rend, diffuse, se
rendort. Les tasks de connexion drainent leurs files. Le plan UDP tourne sans
jamais toucher un shard (§11).

**Ce qu'un opérateur doit voir** : par shard — version courante, nombre de
connexions, durée du rendu, taille des deltas, fréquence des réveils, retard
maximal d'un curseur ; global — nombre de bindings UDP, paquets/s, connexions
refusées par le routeur.

**Questions opérationnelles qui en découlent** (voir §12) : qui crée le premier
shard, que faire si `route()` désigne un shard inexistant (refuser ou créer à la
volée), et où vont les connexions d'un shard détruit (Q9).

### 4.7 `Params`

```rust
pub struct Params {
    /// Le sous-ensemble du shard que cette connexion voit.
    /// DOIT être clos par référence (§7.2).
    mask: VisibilityMask,
    /// Réécritures de champs, fonction pure de (valeur du shard, params).
    rewrites: Rewrites,
    /// Éléments propres à cette connexion, absents du shard.
    private: PrivateOverlay,
}
```

**Pourquoi cette forme.** Le typage rend le partage structurel, non
disciplinaire : `render` ne _peut pas_ produire du contenu par spectateur. Si le
flavor a besoin de personnalisation, il la déclare dans `Params`, où son coût est
visible et mesurable, au lieu de la cacher dans une boucle de rendu.

### 4.8 `Snapshot` disparaît

**Décision proposée : supprimer `Snapshot`, `FlavorRevision`, `SnapshotSource` et
les `Arc<S>`.**

`Snapshot` existait pour **deux** raisons, toutes deux dissoutes par « un shard =
une task = un propriétaire » :

1. _Immutabilité pendant une génération_ — rendre toutes les connexions depuis un
   état figé, sans lecture déchirée. Dans une task mono-propriétaire, rien ne peut
   muter sous le rendu : c'est garanti par `&mut self`, pas par un `Arc`.
2. _Le flavor possède sa concurrence, Mumble Server Runtime ne le verrouille jamais_ — le
   runtime empruntait un `Arc` pour ne pas avoir à prendre de verrou. Sans
   partage, il n'y a plus rien à ne pas verrouiller ; `!Sync` le dit dans le type.

Ce qu'on garde autrement :

- **La révision** devient la version du shard, possédée par le runtime, et non un
  nombre que le flavor doit maintenir.
- **Le replay** (spec P9) se fait mieux au niveau runtime : journaliser les deltas
  **émis** est un artefact plus fidèle qu'un snapshot métier, puisque c'est
  exactement ce que le client a reçu.
- **La communication depuis l'extérieur** (serveur Minecraft → Mumble Server Runtime) passe de
  « publier un `Arc<Snapshot>` + drapeau dirty » à « envoyer un message typé dans
  la mailbox du shard ». C'est plus explicite et ça donne une **backpressure**
  que l'`Arc` partagé n'avait pas.

Coût pour l'intégrateur : son état métier vit désormais **dans** la task de
shard, donc il communique par messages au lieu de partager de la mémoire. C'est
un changement réel, et c'est le bon sens de la dépendance.

### 4.9 Où vivent les `Arc` et les `Mutex`

La proposition ne prétend pas « aucun `Arc` nulle part » — ce serait faux. Elle
prétend trois choses vérifiables :

> **Aucun verrou global. Aucun verrou tenu à travers un `.await`. Aucun verrou
> contendu sur le chemin de contrôle.**

Il y a donc trois emplacements possibles, et chacun a un propriétaire net.

#### a) Le partage métier → **dans le `ShardLogic` concret**, jamais ailleurs

C'est la réponse à « il faut bien que ça soit quelque part ». Si un serveur gRPC
ou un plugin Minecraft doit écrire, c'est le flavor qui porte le mécanisme, et
Mumble Server Runtime ne le voit ni ne le touche. Deux formes, à choisir selon la sémantique :

| forme                                                  | quand                                                                  | conséquence                                                                                |
| ------------------------------------------------------ | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `mpsc::Receiver<Msg>` dans la logique, `Sender` dehors | **flux d'événements** (« le joueur a rejoint l'équipe B »)             | aucun verrou ; `try_recv()` ne bloque jamais la task de shard ; backpressure naturelle     |
| `Arc<Mutex<State>>` cloné dehors                       | **dernière valeur gagnante** (une table de positions réécrite à 20 Hz) | pas de file de valeurs périmées, mais un verrou que la task de shard prend dans `render()` |

**Recommandation par défaut : le canal.** Il rend impossible le mode de panne du
mutex — un writer extérieur qui fait de l'I/O sous le verrou **bloque la task du
shard**, donc retarde le rendu de toutes les connexions qui y sont attachées. Le
mutex reste légitime pour du « dernière valeur gagnante », avec la contrainte déjà
énoncée : **section critique courte, jamais d'`.await` dedans**.

#### b) `ShardHandle` → **aucun état partagé**

C'est le point important, et c'est ce qui empêche la poignée de devenir une porte
dérobée vers l'état du shard :

```rust
pub struct ShardHandle {
    shard: ShardId,
    wake: Arc<Notify>,                    // signal, pas donnée
    cmd: mpsc::Sender<RuntimeCommand>,    // message, pas pointeur
}
```

`move_connection` et `close_connection` **envoient une commande**, ils ne mutent
rien : les tables de shards appartiennent à des tasks. La poignée ne porte donc
que la capacité de _signaler_, jamais un accès à l'état. C'est ce qui la rend
`Clone + Send + Sync` sans aucun verrou.

#### c) Le runtime → des `Arc`, oui, mais tous _read-mostly_ ou non contendus

| structure                       | écrite par                                     | lue par                      | nature                                                                                 |
| ------------------------------- | ---------------------------------------------- | ---------------------------- | -------------------------------------------------------------------------------------- |
| `ArcSwap<Bindings>`             | association UDP, déconnexion, migration (rare) | plan UDP (chaque datagramme) | copy-on-write, lecture lock-free                                                       |
| `Arc<ArcSwap<ShardRouting>>`    | la task du shard                               | plan UDP                     | **le seul cellier partagé entre un shard et le hot path**, et il est en `store`/`load` |
| `Arc<Mutex<CryptState>>`        | plan UDP, tunnel TCP                           | idem                         | **par connexion**, donc jamais contendu ; l'état OCB2 est mutable par nature           |
| `Arc<AtomicU64>` (curseur)      | la task d'écriture de la connexion             | plan UDP (gating inv. 19)    | atomique, pas de verrou                                                                |
| `mpsc::Sender` (file de sortie) | task de shard, plan UDP                        | task d'écriture              | canal borné                                                                            |

Aucune de ces lignes n'est un verrou global, et aucune n'est prise par le plan de
contrôle. C'est ce que garantit l'énoncé en tête de §4.9 — pas l'absence d'`Arc`.

---

## 5. Le pipeline

```
              ┌──────────────────────────────────────────────┐
  dirty ─────▶│  boucle de réconciliation (1 par Shard)      │
  tick        │                                              │
  message     │  logic.render()                     O(W)     │
              │  diff(head, rendered)               O(W)     │
              │  encode → Arc<[u8]>                 O(|D|)   │
              │  append au journal, head += 1                │
              └──────────────────┬───────────────────────────┘
                                 │  delta partagé, déjà encodé
              ┌──────────────────▼───────────────────────────┐
              │  composition par connexion attachée  O(|D|)  │
              └──────────────────┬───────────────────────────┘
                                 │
        ┌────────────────────────┼────────────────────────┐
        ▼                        ▼                        ▼
   conn 1 (cursor, params)  conn 2 (cursor, params)  conn 3 (cursor, params)
```

### 5.1 Composition par connexion : les quatre termes

```
D_c =  filter_rewrite(D_shard, params_c)     ← travail partagé, O(|D_shard|)
     ⊕ visibility_delta(S_c → S'_c)          ← 0 en régime établi
     ⊕ param_delta(params_c → params'_c)     ← 0 en régime établi
     ⊕ diff(private_c, private'_c)           ← petite réconciliation locale
```

Le terme 1 est le cas courant. Les trois autres ne se déclenchent que quand
quelque chose change **pour cette connexion précise** :

- **Terme 2 — changement de visibilité.** Quand `S_c` change, les éléments qui
  entrent sont dans le shard mais n'ont **jamais été envoyés** à `c` ; ils
  n'apparaissent donc pas dans `D_shard` (ils n'ont pas changé globalement) et
  aucun filtrage ne peut les produire. Il faut le terme explicite :
  `Add(W|_{S'\S})` et `Remove(S\S')`, coût O(|S Δ S'|), inhérent à ce que ce
  joueur doit réellement apprendre. **Un changement de shard est ce terme dans sa
  forme maximale** (§10.3).
- **Terme 3 — changement de params.** Si un joueur change d'équipe et que les
  libellés sont relatifs à l'équipe, le shard n'a pas bougé mais son rendu si.
- **Terme 4 — overlay privé.** Vraie réconciliation, mais sur quelques éléments.

### 5.2 Condition de correction

Envoyer `f_c(D)` n'est valide que si `f_c` se distribue sur diff/apply :

```
f_c(W) == apply( f_c(C_partagé), f_c(diff(C_partagé, W)) )
```

- **Filtre à masque fixe** : vrai. Le diff est champ à champ, donc restreindre le
  diff équivaut à diffuser la restriction.
- **Réécriture** : vrai **si** l'override est une fonction pure de
  `(valeur du shard, params)`. Si le champ n'est pas dans le delta, il n'a pas
  changé globalement, donc l'override non plus.
- **Sinon** : c'est un terme 2 ou 3, explicite.

Les termes 2 et 3 ne sont pas des rustines : ils sont exactement la partie de
`f_c` qui ne commute pas.

---

## 6. Ordonnancement

Le contrôle n'est pas partagé, donc rien n'est verrouillé : **une task possède
tout l'état de contrôle d'un shard**, et le reste communique par message.

Le rythme, dans la boucle de §3.3 :

```rust
dirty.notified().await;                 // latence nulle à vide
drain_commands();                       // observe() immédiat
drain_until(next_publication).await;     // immédiat après une période inactive
reconcile();                            // O(W), au plus 20 fois par seconde
next_publication = Instant::now() + MIN_INTERVAL;
```

Réponse immédiate à vide, mise en lot automatique sous charge, plafond dur sur le
taux de publication. Les quatre sites d'appel actuels de `publish_generation()`
se réduisent à **un signal**, et le réveil peut venir de trois sources : le
flavor (`ctl.dirty()`), le runtime (`RuntimeHandle::wake`), ou le tick.

**Deux boucles, pas une.** La réconciliation de vue et le raffinement des routes
audio tournent à des fréquences différentes sur des données différentes. La
séparation est démontrable, pas seulement pratique :

- les **changements d'attachement / de masque** appartiennent à la boucle de vue,
  qui porte l'ordre de sûreté (révocation avant retrait, activation après ajout) ;
- le **raffinement intra-shard** (proximité) ne fait que resserrer ou élargir
  l'audio entre gens **déjà visibles**, donc il ne peut pas violer un invariant
  couplé à la visibilité, et tourne sur son propre tick.

### 6.1 À supprimer

`connection.rs:200` (republication globale à chaque frame drainée) disparaît : le
drainage n'avance qu'un curseur, ce qui est une opération locale à la connexion.

---

## 7. Règles de correction

### 7.1 La règle d'avancement unique

> **Chaque envoi avance exactement un de `cursor_c` ou `params_c`.**

Avancer le curseur → envoyer le delta shard filtré. Changer les params → envoyer
le delta de visibilité/params **au curseur courant**. Jamais les deux en une
étape. Sous cette règle, l'état dérivé reste cohérent par construction.

**Oracle pour le testkit** (descendant direct du proptest P5
`apply(committed, plan) == desired`) :

```
état du ClientModel  ==  f_{params_c}( Shard[cursor_c] ) ⊕ private_c
```

Proptest : entrelacer aléatoirement des ticks de shard, des changements de params
et des migrations, vérifier l'égalité après chaque envoi.

### 7.2 Le masque doit être clos par référence

Filtrer des opérations n'est **pas** local : supprimer `CreateChannel(X)` oblige
à supprimer aussi `AddUser(u, channel=X)`, les `links` vers X, les listeners sur
X. Fait au cas par cas, ce sera faux.

On en fait donc une propriété du masque, validée une fois à sa construction :

> `S_c` est clos par la relation de référence : voir un utilisateur implique voir
> son canal ; voir un canal implique voir ses ancêtres jusqu'à la racine.

Alors le filtrage est un test local par opération et **ne peut pas** produire une
référence pendante.

### 7.3 Deux propriétés gratuites

- **Le filtrage préserve l'ordre.** Toutes les règles d'ordonnancement du planner
  sont des contraintes « X avant Y ». Retirer des éléments d'une séquence ne viole
  jamais une contrainte d'antériorité. Un plan valide filtré reste valide :
  **aucune replanification**. À encoder en test.
- **L'injection a besoin d'un emplacement.** Ajouter des opérations privées est
  une insertion, qui _peut_ violer l'ordre. On garde les éléments privés dans leur
  propre sous-arbre et on append : la position devient trivialement sûre.

---

## 8. Le journal, pas le cache

Une première formulation naturelle est `Map<(View, CommittedState), ToSend>` :
mémoïser le diff pour ne pas le recalculer N fois. L'idée est juste, la clé ne
l'est pas — hacher une vue engagée entière coûte O(V) par consultation.

Les états engagés forment une **chaîne**, donc on indexe par `u64` :
`Map<(v_from, v_to), Arc<[u8]>>`. Et on constate qu'on a écrit un **journal de
deltas** : on stocke les deltas consécutifs, on rejoue `cursor → head`, et on
retombe sur un **snapshot complet** au-delà d'un seuil de profondeur. Mémoire
bornée, consultation O(1), et un seul chemin pour cinq situations :

| événement                        | traitement                                               |
| -------------------------------- | -------------------------------------------------------- |
| tick, rien n'a changé            | un rendu + diff vide. **Zéro travail par connexion.**    |
| changement                       | O(1) diff, O(1) encodage, puis O(N) clones d'`Arc`       |
| connexion qui arrive             | snapshot au head, `cursor = head`                        |
| connexion en retard              | le curseur reste derrière, rejoue au drainage            |
| connexion tombée hors du journal | snapshot au head — **même chemin que l'arrivée**         |
| connexion qui change de shard    | détacher, snapshot du nouveau shard — **encore le même** |

Mémoïser `filter(D, mask)` entre connexions partageant un masque redevient une
**optimisation** ajoutable plus tard si la mesure la justifie, et non un mécanisme
de correction.

### 8.1 Ce qui disparaît

`PendingTransition`, `CommitToken`, `PublicationCommit`, `PublishedGeneration`,
les epochs, `StalePublication`, `PublicationCoordinator`, le mutex du
coordinateur, `Snapshot`/`SnapshotSource`/`FlavorRevision`. L'état engagé d'une
connexion **est** son curseur : un `u64`. Le commit atomique reste `try_reserve`
de tout le delta puis avancée du curseur — ce qui existe déjà dans `outbound.rs`.

La propriété de convergence survit intacte : une connexion dont la file a refusé
`v5` et qui voit maintenant `head = v9` rejoue 5→9, ou prend un snapshot à 9 —
en sautant les intermédiaires.

---

## 9. Audio

Les routes se recompilent par **shard**, jamais par connexion, et n'ont pas
besoin d'une passe de fine-tune séparée. Une seule règle :

> Une route est autorisée par le shard, puis masquée par le destinataire :
> `shard_allows(s → r) && s ∈ S_r`.

Un test d'appartenance dans le hot path, et **le même `S_c`** qui pilote le
filtrage de vue pilote le filtrage audio. Une structure, deux consommateurs.

**Invariant 19 sous curseurs.** La table de routage avance au head ; une connexion
en retard est derrière. Chaque route porte donc `valid_from: u64`, et le hot path
teste `cursor_r >= route.valid_from` — un chargement atomique et une comparaison,
sans allocation. La révocation reste **eager et inconditionnelle** (entendre
_moins_ que son dû est toujours sûr) ; l'activation est **gated** sur le curseur.
C'est l'asymétrie déjà établie par le design actuel, exprimée par une comparaison
de `u64` au lieu d'un protocole de commit.

`mumble-server-runtime-audio::compile` est quadratique par construction (« toute paire est une
route », 13,5 µs à 128 participants) : acceptable par shard. La proximité
demandera un index spatial pour retomber en O(n) par tick — boucle séparée, elle
ne touche pas les vues.

---

## 10. Possession, migration, IDs

### 10.1 Qui possède quoi

C'est la décision structurante, et elle sépare deux choses que le mot
« posséder » confond :

| objet                                                                       | propriétaire             | pourquoi                                                    |
| --------------------------------------------------------------------------- | ------------------------ | ----------------------------------------------------------- |
| **état de contrôle** d'une connexion (`cursor`, `params`, émetteur de file) | **le Shard**             | c'est ce qui doit être cohérent avec la version du shard    |
| **socket TLS, crypto OCB2, boucle de lecture/écriture**                     | **la task de connexion** | une écriture socket await ; un shard ne doit jamais awaiter |

Le shard _possède_ la connexion au sens de l'autorité et de l'ordonnancement ; il
ne détient pas le descripteur. C'est ce qui permet à la task de shard de rester
purement CPU, et à un client lent de ne ralentir que lui-même.

### 10.2 Ce que ça change pour la migration

Intra-processus, déplacer une connexion revient à **envoyer une valeur d'une task
à une autre** : l'état de contrôle est une petite structure `Send`. Le socket ne
bouge pas du tout, puisqu'il n'a jamais appartenu au shard.

C'est seulement **inter-processus** que le problème apparaît, et il ne porte pas
sur l'état de contrôle (sérialisable) mais sur le socket TLS. Trois options
connues, à trancher plus tard (Q7) : reconnexion visible, passage de descripteur
(même machine), ou frontal qui possède les sockets et relaie les frames.

### 10.3 Protocole de migration

Déplacer `c` du shard A vers le shard B :

1. **A révoque l'audio.** Recompile son routage sans `c` et swap. `c` cesse
   d'entendre A et A cesse d'entendre `c`. Eager, inconditionnel, sûr par
   direction (invariant 18).
2. **A pousse le delta de retrait** dans la file de `c` : `Remove(S_c)`.
3. **A retire `c` de sa table** et envoie l'état de contrôle dans la mailbox de B.
4. **B calcule les `Params`**, pousse un snapshot de `S'_c` au head de B, insère
   `c` avec `cursor = head_B`, recompile son routage avec `c`.
5. **B accorde l'audio**, gated sur `cursor_c >= valid_from` (invariant 19).

**Ordre garanti sans protocole supplémentaire** : A pousse (2) _avant_ d'envoyer
(3), B pousse (4) _après_ avoir reçu (3), et la file de sortie de `c` est FIFO.
Le client voit donc le retrait puis l'ajout, jamais l'inverse.

**Commandes entrantes pendant la migration.** La task de connexion tient un
`ArcSwap<ShardMailbox>` mis à jour par B à l'attachement. Un message qui atterrit
dans A pour une connexion qu'il ne possède plus est **jeté avec un log** : une
commande entrante est consultative (elle devient un `VoiceEvent`), et en perdre
une pendant une migration est fail-closed et acceptable. Pas de protocole de
transfert.

Entre 1 et 5, la connexion n'entend personne : silence bref et fail-closed. Un
paquet voix en vol de `c` routé sous la table de A et arrivant après le
détachement ne trouve plus `c` parmi les membres et est jeté — correct.

**Test d'élégance passé : migration = `detach` + `attach`, deux opérations déjà
nécessaires pour l'arrivée et le départ. Elle n'introduit aucun mécanisme.**

### 10.4 Allocation d'IDs entre shards

La migration crée un problème d'IDs : si A a le canal 7 = « Lobby » et B le canal
7 = « Arena », le client voit l'ID 7 changer d'identité — violation de
l'invariant 12.

**Décision proposée : IDs uniques globalement, alloués par blocs.** Chaque shard
loue un bloc (p. ex. 4096) auprès d'un allocateur monotone, alloue localement, et
redemande un bloc à l'épuisement. Zéro contention dans le chemin courant, unicité
globale, aucune coordination hors du bail. Même schéma pour les session IDs des
utilisateurs (aujourd'hui déjà un `AtomicU32` global de processus).

**La racine est particulière.** Mumble exige que le canal racine soit l'ID `0`.
La racine n'appartient donc à **aucun shard** : elle appartient au runtime, et les
shards sont des sous-arbres sous elle. Conséquence agréable : lors d'une
migration, la racine ne bouge pas, seul le sous-arbre est échangé — pas de
clignotement de l'arbre entier côté client.

⚠️ Dépend de la question ouverte Q1 (§12).

### 10.5 Multi-processus : ce qu'il faut préserver

Le v1 est **mono-processus** : les shards sont des tasks tokio. Cinq règles
gardent la porte ouverte :

1. Aucune référence croisée entre shards dans l'état.
2. IDs uniques globalement (§10.4), pour qu'une connexion porte son historique
   d'IDs à travers les shards.
3. La communication shard → connexion est un **flux d'octets** (le journal de
   deltas), jamais un parcours de pointeurs partagés.
4. Les tables de routage sont des **valeurs répliquées**, pas un service
   interrogé. **Le shard est une autorité de plan de contrôle, jamais un relais
   de plan de données** (ADR-005).
5. L'état de contrôle d'une connexion est une petite structure sérialisable ;
   seul le socket pose un problème de transfert.

---

## 11. Le plan UDP

C'est le point sensible, parce que le TCP est trivial (la task de connexion
possède son socket, donc le routage entrant est direct) alors que l'UDP arrive
sur **un socket partagé** sans rien qui dise à qui il appartient.

### 11.1 La contrainte

Un datagramme n'est identifiable que par son `SocketAddr` source. Il faut, sans
verrou et sans passer par une task de shard :

1. trouver à quelle connexion il appartient ;
2. le déchiffrer ;
3. trouver ses destinataires ;
4. les chiffrer et les envoyer.

Faire (3) en interrogeant la task du shard mettrait le plan de contrôle sur le
chemin de la voix — interdit par ADR-005 et par les gates R4.

### 11.2 Structure proposée

Une table globale en lecture quasi-exclusive, échangée par `ArcSwap` :

```rust
/// Écrit à l'association UDP, à la déconnexion, à la migration. Lu à chaque
/// datagramme. Copy-on-write : les écritures sont rares, les lectures sont
/// le hot path.
type Bindings = HashMap<SocketAddr, Binding>;

struct Binding {
    connection: ConnectionId,
    session: SessionId,
    /// L'état OCB2 de CETTE connexion. Mutable par nature (IV, anti-rejeu),
    /// donc un mutex par connexion — jamais partagé, jamais contendu.
    crypt: Arc<Mutex<CryptState>>,
    /// La table de routage du shard auquel cette connexion est attachée.
    /// Le shard y publie par `store`, le plan UDP la lit par `load`.
    routing: Arc<ArcSwap<ShardRouting>>,
}
```

`ShardRouting` est **auto-suffisant** — c'est la conséquence utile de « l'audio ne
franchit pas une frontière de shard » : tous les destinataires possibles d'un
émetteur sont dans le même shard, donc la table peut porter directement leurs
poignées de livraison.

```rust
struct ShardRouting {
    /// Le snapshot pur de `mumble-server-runtime-audio` : qui peut entendre qui.
    /// Le crate reste pur : il ne connaît ni file, ni socket (gates R4).
    audio: AudioRoutingSnapshot,
    /// Parallèle au même index de session : où livrer.
    delivery: Vec<DeliveryHandle>,
}

struct DeliveryHandle {
    udp_addr: Option<SocketAddr>,        // None ⇒ repli tunnel TCP
    queue: OutboundSender,               // la file de la connexion
    crypt: Arc<Mutex<CryptState>>,
    cursor: Arc<AtomicU64>,              // pour le gating de l'invariant 19
}
```

Le chemin chaud devient, **sans aucune task de shard impliquée** :

```
recv_from  →  bindings.load().get(&addr)        1 hash, lock-free
           →  decrypt (mutex de CETTE connexion)
           →  binding.routing.load()            1 atomic
           →  receivers(sender, target)         slice empruntée, 0 alloc
           →  pour chaque destinataire : cursor >= valid_from ? encrypt → send
```

C'est exactement la forme du hot path actuel (P4 : ~8,5 ns/destinataire en
consultation, ~21 ns en relais complet), avec une indirection de plus (`routing`
au lieu d'un snapshot global) et zéro verrou supplémentaire.

### 11.3 Le pré-filtrage par IP : chemin froid seulement

L'intuition « pré-filtrer par IP » est juste, mais elle appartient au chemin
**froid**, celui de l'association initiale — et le design est déjà en place
depuis P2/P3, tracé dans Murmur (R1) :

- **adresse connue** : lookup exact `SocketAddr` → `Binding`. O(1). C'est 99,99 %
  du trafic.
- **adresse inconnue** : les candidats sont les connexions dont l'**IP hôte TCP**
  correspond (`qhHostUsers`), et on lie l'adresse au **premier `checkDecrypt` qui
  réussit** (`qhPeerUsers`). Borné par le nombre d'utilisateurs derrière un même
  NAT.

Ce qui rend cet essai-erreur sûr est un fait vérifié en source : **un `decrypt`
OCB2 en échec est sans effet de bord** (l'IV est restauré, aucune écriture
d'historique de rejeu), donc essayer un datagramme contre plusieurs domaines
candidats ne les corrompt pas.

Une seule chose change avec le sharding : l'index **IP hôte → connexions** doit
être **runtime-global** et non par shard, puisqu'on ne sait pas encore à quel
shard appartient l'émetteur. Il est petit et n'est écrit qu'à la
connexion/déconnexion.

### 11.4 La migration ne perturbe pas l'UDP

Point rassurant : une migration de shard ne change **ni** l'adresse source, **ni**
la clé OCB2, **ni** la session. Elle ne change que le champ `routing` du
`Binding`. L'association UDP survit donc telle quelle, et le seul effet est que
les paquets suivants sont routés dans la table du nouveau shard — ce qui est
précisément l'effet voulu.

### 11.5 Multi-processus (Q7)

Puisque le lookup est un pur hash et que les tables de routage sont des valeurs
répliquables, **n'importe quel processus peut router n'importe quel paquet s'il a
les tables**. Les formes viables :

- `SO_REUSEPORT` + programme BPF de steering par adresse source, pour que le
  datagramme atterrisse directement sur le processus qui possède la connexion ;
- à défaut, un saut de transfert interne, le processus destinataire chiffrant et
  écrivant (le chiffrement est par connexion, donc il ne peut pas être fait
  ailleurs).

Ce qu'on ne fait **jamais** : router la voix à travers la task ou le processus du
shard.

---

## 12. Questions ouvertes

| #       | question                                                                                                                                        | pourquoi elle bloque                                                                                               | statut                                         |
| ------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------- |
| **Q1**  | Le client Mumble officiel tolère-t-il des IDs de canaux **épars et grands** ? S'il les utilise comme indices de tableau, §10.4 s'effondre.      | Réponse R1, dans les sources vendorées. **La moins chère et la plus structurante.**                                | ouverte                                        |
| **Q2**  | Peut-on écrire `Params` pour 2–3 scénarios Minecraft réels sans y mettre l'identité du joueur ?                                                 | Si le terme 3 se déclenche en permanence, le gain en régime établi s'érode. **À falsifier en premier, sans code.** | ouverte                                        |
| **Q3**  | La proximité reste-t-elle hors du VDOM ?                                                                                                        | Une position par paire, mise dans la vue, force une personnalisation par joueur.                                   | proposée : oui, routage seulement, tick séparé |
| **Q4**  | Qui construit le masque : le flavor le déclare (`params`), ou le runtime le dérive du rendu ?                                                   | Déclaré est plus simple et plus honnête, mais déplace une charge de correction vers l'auteur du flavor.            | ouverte                                        |
| **Q5**  | Profondeur du journal et seuil de snapshot ; mémoire par shard.                                                                                 | Dimensionne le pire cas d'un client lent.                                                                          | ouverte                                        |
| **Q6**  | Les IDs globaux fuitent la cardinalité. Les session IDs le font déjà. Acceptable ?                                                              | Décision de sécurité explicite.                                                                                    | ouverte                                        |
| **Q7**  | Migration inter-processus : reconnexion, passage de fd, ou frontal ? Et steering UDP (`SO_REUSEPORT` + BPF) ?                                   | À différer, mais écrire quelle porte on garde ouverte (§10.5, §11.5).                                              | différée                                       |
| **Q8**  | Utilisateurs synthétiques (spec 9.4), aujourd'hui refusés. L'overlay privé de `Params` leur donne-t-il enfin une place ?                        | Bloquant hérité de P7, à trancher avant P8.                                                                        | ouverte                                        |
| **Q9**  | Que devient une connexion quand son shard est détruit sous elle : repli vers un shard « lobby », ou fermeture ?                                 | Politique, donc au flavor — mais le runtime doit offrir un défaut sûr.                                             | ouverte                                        |
| **Q10** | Une task de shard qui panique ferme ses connexions. Faut-il un `catch_unwind` autour de l'appel au flavor pour survivre à un bug métier isolé ? | Compromis robustesse / fail-closed.                                                                                | ouverte                                        |

---

## 13. Ce qui survit, ce qui change

**Intact.** `mumble-server-runtime-protocol`, `mumble-server-runtime-crypto`, `mumble-server-runtime-audio` (le hot path pur
est bon, et §11.2 le préserve mot pour mot), le catalogue d'invariants §20, le
juge du testkit, les oracles corpus/proxy, la séparation R2, les gates R4 — et
surtout **la prémisse déclarative** : `render → diff → plan ordonné` ne change
pas. On ne renonce pas à la bonne idée ; on change ce à quoi elle s'applique.

**Réécrit.** L'unité de rendu (`ShardLogic::render` au lieu de
`render(connection)`), l'ordonnancement (une task par shard au lieu de quatre
sites d'appel), l'allocation d'IDs (bail par bloc, unique globalement), la
publication (journal + curseur au lieu de coordinateur + jetons), et
l'authentification (le credential va au `ConnectionRouter` au lieu d'être jeté).

**Supprimé.** Le protocole de commit multi-connexions et son coordinateur (§8.1),
la republication globale au drainage (§6.1), et `Snapshot`/`SnapshotSource`/
`FlavorRevision` (§4.8).

**Décompte de concepts** — la vraie métrique KISS :

- aujourd'hui : `DesiredClientView`, `ClientView`, `ViewIdMapping`, `ViewDelta`,
  `ChannelPatch`, `UserPatch`, `OutputTransaction`, `PlanOp`, `EmittedStep`,
  `PendingTransition`, `CommitToken`, `PublicationCoordinator`,
  `PendingPublication`, `PublicationCommit`, `PublishedGeneration`,
  `RenderedSnapshot`, `ValidatedSnapshot`, `Snapshot`, `FlavorRevision` — **19** ;
- proposition : `Shard`, journal de versions, `Cursor`, `Params` — **4** (plus le
  vocabulaire diff/plan conservé de P5, plus `ConnectionRouter`/`ShardControl` qui
  sont des poignées, pas des états).

### 13.1 Le critère : un crate existe s'il y a une arête à interdire

Les gates R4 (`ci/dep-direction.sh`) travaillent par frontière de crate. Un crate
n'est donc pas une commodité de rangement : **c'est le seul moyen d'imposer
mécaniquement une règle d'architecture**. On n'en crée un que s'il existe une
dépendance qu'on veut rendre impossible à écrire.

### 13.2 Carte de crates proposée à l'époque (non retenue)

```
PURS (ni tokio, ni socket, gates R4)
  mumble-server-runtime-protocol    codec wire                          inchangé
  mumble-server-runtime-crypto      OCB2                                inchangé
  mumble-server-runtime-render      types de vue, normalize, validate   quasi inchangé
  mumble-server-runtime-reconcile   diff, plan, ordre de sûreté         quasi inchangé ★
  mumble-server-runtime-audio       snapshot de routage, politique      inchangé ★
  mumble-server-runtime-session     emit (PlanOp → wire), inbound       amputé de view.rs
  mumble-server-runtime-project     masque, filtrage, réécriture,       NOUVEAU ★
                      composition en quatre termes
  mumble-server-runtime-journal     versions, anneau de deltas,         NOUVEAU
                      curseurs, repli snapshot
  mumble-server-runtime-flavor      ShardLogic, ConnectionRouter,       réécrit
                      Params, VoiceEvent

IMPUR
  mumble-server-runtime-runtime     ordonnanceur de shards (une task par shard), mailboxes,
                      migration, plan UDP, tasks de connexion, files bornées,
                      allocateur d'IDs                    ex-mumble-server-runtime-server

COMPOSITION
  tools/mumble-server-runtime-*     binaire = runtime + flavor concret + routeur

VÉRIFICATEUR (R2)
  mumble-server-runtime-testkit     SimulatedMumbleClient + nouvel oracle
```

★ = les trois cœurs qui portent la valeur du système.

**Arêtes interdites nouvelles**, à ajouter aux gates :

- `mumble-server-runtime-project` ↛ `mumble-server-runtime-protocol` — on filtre des vues, on encode après.
- `mumble-server-runtime-journal` ↛ tout le reste — c'est une structure de données générique
  sur sa charge utile ; qu'elle ne connaisse **aucun** vocabulaire de vue est
  précisément ce qui la rend proptestable isolément.
- `mumble-server-runtime-flavor` ↛ `mumble-server-runtime-protocol` (déjà en place), et ↛ `mumble-server-runtime-journal`
  (un flavor n'a pas à connaître le versionnement).

### 13.3 Ce qui disparaît, en lignes

| fichier                                  | lignes | sort                                                        |
| ---------------------------------------- | ------ | ----------------------------------------------------------- |
| `mumble-server-runtime-control/src/publication.rs`     | 1080   | supprimé (coordinateur, jetons, epochs)                     |
| `mumble-server-runtime-control/src/validation.rs`      | 609    | ~supprimé (la confidentialité devient la clôture du masque) |
| `mumble-server-runtime-control/src/mumble_server_runtime_control.rs` | 227    | supprimé                                                    |
| `mumble-server-runtime-session/src/view.rs`            | 579    | supprimé (remplacé par un `u64`)                            |
| `mumble-server-runtime-control/src/voice_events.rs`    | 466    | **survit**, déménage                                        |
| `mumble-server-runtime-session/src/emit.rs`            | 866    | **survit**                                                  |
| `mumble-server-runtime-session/src/inbound.rs`         | 551    | **survit**, résout contre le masque                         |
| `mumble-server-runtime-server/src/outbound.rs`         | 483    | **survit** (la file bornée + `try_reserve` est bonne)       |

Environ **2 500 lignes supprimées**, contre ~700–900 à écrire (`project` +
`journal`). Le crate `mumble-server-runtime-control` disparaît entièrement.

### 13.4 Un oracle par composant

C'est là que la robustesse se gagne vraiment : **on ne peut tester que ce qu'on
sait nommer.** Le modèle actuel laisse implicites des choses qui deviennent des
valeurs, et chaque valeur devient testable seule.

| aujourd'hui, implicite                                                                            | demain, nommé                              | propriété vérifiable seule                                                                                        |
| ------------------------------------------------------------------------------------------------- | ------------------------------------------ | ----------------------------------------------------------------------------------------------------------------- |
| la projection d'une vue, cachée dans `render()`                                                   | `Params` + `filter`                        | `apply(filter(D)) == filter(apply(D))` ; masque clos ⇒ aucune référence pendante ; le filtrage préserve l'ordre   |
| « jusqu'où le client a été informé », éclaté entre `revision`, `CommitToken` et l'état de la file | `cursor: u64`                              | sémantique d'anneau : avancée, chute hors fenêtre, équivalence rejeu/snapshot — **sans aucun vocabulaire de vue** |
| « qui possède une connexion » : personne (`SharedState` est un sac)                               | le Shard                                   | isolation de panne, migration = detach+attach                                                                     |
| « quand rend-on » : 4 sites d'appel                                                               | une boucle                                 | coalescence, plafond de débit, latence à vide                                                                     |
| la confidentialité, validée sur **chaque sortie**                                                 | la clôture du masque, validée **une fois** | surface de vérification divisée par le nombre d'éléments                                                          |

Et les couches basses gardent leurs oracles existants sans y toucher :
`mumble-server-runtime-reconcile` son proptest 4000 graines, `mumble-server-runtime-audio` ses propriétés et
son bench, `mumble-server-runtime-protocol`/`mumble-server-runtime-crypto` le corpus et les vecteurs.
L'oracle composé du runtime (§7.1) ne fait que les empiler.

### 13.5 Ce qu'on ne découpe **pas**

- `mumble-server-runtime-render` et `mumble-server-runtime-reconcile` restent séparés parce qu'ils le sont
  déjà et que ça marche, mais aucune arête interdite ne le justifie : les fusionner
  serait légitime, les re-découper autrement ne l'est pas.
- `emit` et `inbound` restent dans `mumble-server-runtime-session` : un seul crate qui connaît
  les deux vocabulaires (vue et wire), c'est le point du crate.
- Pas de crate « sharding » séparé : le shard **est** l'unité d'ordonnancement du
  runtime, donc il vit avec les tasks. Un crate pur ne pourrait pas le contenir.
- Pas de crate « scheduler » : la boucle fait cinq lignes (§3.3). Un crate pour
  cinq lignes est exactement l'usine qu'on refuse.

### 13.6 Où le risque se déplace

Honnêteté du bilan : la refonte ne supprime pas le risque, elle le **déplace**,
et il faut savoir où il atterrit.

| gagné                                                                                                                  | perdu / déplacé                                                                                 |
| ---------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| Confidentialité **structurelle** (on ne peut pas adresser ce à quoi on n'est pas abonné) au lieu de validée par sortie | Le **contenu** du masque reste une décision du flavor. On valide sa clôture, pas sa pertinence. |
| Rayon de panne : un shard, pas le serveur                                                                              | Le flavor tourne **dans** la task de shard : bloquer ou paniquer y est plus grave (Q10)         |
| Un client lent est un `u64` en retard, il ne peut affecter personne                                                    | —                                                                                               |
| 19 concepts → 4 : beaucoup moins d'états invalides représentables                                                      | IDs globaux épars (Q1)                                                                          |
| Un oracle par couche, testable sans les couches du dessus                                                              | Le transport distribué (annexe A) est de l'infrastructure neuve                                 |

---

## 14. Complexité

|                     | régime établi      | changement de masque | rechargement complet |
| ------------------- | ------------------ | -------------------- | -------------------- |
| rendu + diff shard  | O(W)               | O(W)                 | O(W)                 |
| par connexion       | O(\|D\|)           | O(\|S Δ S'\|)        | O(W)                 |
| **total par shard** | **O(W + N·\|D\|)** | + O(Σ churn masques) | O(N·W)               |

En régime établi `|D|` vaut quelques opérations, donc **O(W + N)** — linéaire, et
**indépendant du nombre de vues distinctes**. Les shards étant indépendants et
sur des tasks séparées, K shards se répartissent sur K cœurs.

|             | rendu  | diff   | encodage | fan-out | total                                   |
| ----------- | ------ | ------ | -------- | ------- | --------------------------------------- |
| aujourd'hui | O(N·V) | O(N·V) | O(N·V)   | O(N)    | **O(N²)**, mono-verrou                  |
| proposition | O(W)   | O(W)   | O(\|D\|) | O(N)    | **O(W + N·\|D\|)**, parallèle par shard |

Le terme de fan-out ne disparaît jamais : il faut écrire des octets sur N sockets.
C'est le plancher, et c'est pourquoi O(N) par tick est optimal et pas seulement
meilleur.

---

## Annexe A — Scaling horizontal : shards distribués et proxies

> **Exploratoire. Pas à l'ordre du jour.** Consigné pour ne pas être re-dérivé, et
> surtout pour vérifier que le design v1 ne ferme aucune porte. Rien ici ne doit
> être construit avant qu'une mesure ne l'exige.

### A.1 La topologie

Un shard = une machine qui possède les données. Un étage de proxies qui possèdent
les connexions clientes, façon BungeeCord.

```
   clients Mumble
        │  TLS + UDP
   ┌────▼────┐   ┌─────────┐   ┌─────────┐
   │ Proxy 1 │   │ Proxy 2 │   │ Proxy 3 │   ← possèdent socket, TLS, OCB2,
   └──┬───┬──┘   └──┬───┬──┘   └──┬───┬──┘     cursor, Params, encodage wire
      │   └─────────┼───┼─────────┼───┘
      │  voix proxy→proxy (O(M), pas O(R))
      │             │   │         │
      │   journal de deltas + Params (abonnement)
   ┌──▼─────────────▼┐ ┌▼─────────▼──────┐
   │    Shard A      │ │    Shard B      │  ← possèdent l'état métier, le rendu,
   │  (une machine)  │ │  (une machine)  │    le journal, la table de routage
   └─────────────────┘ └─────────────────┘
```

**Le proxy est la promotion de la « task de connexion » (§3.3) au rang de
processus/machine.** Le shard est la promotion de la « task de shard ». La
frontière est exactement la même, seul le transport change — ce qui est le test
que les cinq règles de §10.5 étaient les bonnes.

### A.2 Ce qui traverse la frontière

Deux flux descendants, tous deux rares :

1. **Le journal de deltas du shard**, en _structuré_ (pas encore en frames
   Mumble), diffusé une fois par **proxy** — pas par connexion. Trois proxies et
   500 connexions ⇒ 3 copies, pas 500.
2. **Les mises à jour de `Params`** par connexion, qui ne partent que sur les
   termes 2 et 3 de §5.1, donc quasi jamais en régime établi.

Le proxy fait la composition par connexion (§5.1), l'encodage wire et le
chiffrement. C'est ce qui distribue le CPU au bon endroit et met la
personnalisation là où sont les sockets.

> Ce découpage n'est possible que parce que `Params` est **une donnée** (masque,
> réécritures, overlay) et non une closure. Décision prise en §4.7 pour d'autres
> raisons ; elle se révèle être ce qui rend le modèle distribuable.

Un flux montant : les commandes clientes résolues, qui deviennent des
`VoiceEvent` dans la mailbox du shard.

### A.3 La voix : proxy → proxy, jamais par le shard

Le point le plus important, et il tient parce que **l'audio ne franchit pas une
frontière de shard**.

Le shard réplique sa table de routage (`ShardRouting`, §11.2) à tous les proxies
qui détiennent un de ses membres. Elle est petite et change rarement. Ensuite :

```
A parle (proxy P1)
  → P1 déchiffre avec la clé de A (il la possède)
  → P1 consulte la table du shard de A
  → destinataires locaux : chiffre et envoie directement
  → destinataires distants : UN paquet vers chaque proxy concerné
      → P2 refait sa propre consultation pour SES membres, chiffre, envoie
```

Conséquence à retenir : **le coût inter-proxy est O(M) — le nombre de proxies —
et non O(R), le nombre de destinataires.** À 5 proxies, un locuteur coûte 4
paquets internes, quel que soit le nombre d'auditeurs. Chaque proxy route pour
ses propres connexions à partir de la table répliquée.

Le chiffrement étant par connexion, il ne _peut_ se faire que chez le propriétaire
du socket : c'est ce qui impose ce découpage plutôt qu'un relais central.

**Le gating de l'invariant 19 est local et gratuit.** `cursor_r >= valid_from` se
teste sur le proxy qui possède `r`, exactement là où le chiffrement a lieu. Aucune
coordination.

### A.4 La migration devient plus propre qu'un BungeeCord

BungeeCord doit refaire un login complet vers le backend et masquer la transition
par un changement de dimension. Ici, rien de tel : le proxy se désabonne du
journal de A, s'abonne à celui de B, reçoit un snapshot, et applique
`Remove(S_c)` puis `Add(S'_c)`. **Le client ne voit qu'un gros delta.** Pas de
reconnexion, pas de renégociation TLS, pas de nouvelle clé OCB2.

**Le socket ne change jamais de propriétaire, à aucune échelle.** C'est le point
qui fait tomber le problème que §10.5 (règle 5) laissait ouvert : le proxy est le
domicile permanent de la connexion, les shards sont ce à quoi elle s'abonne.

Raffinement d'ordonnancement imposé par le réseau : la garantie FIFO de §10.3
reposait sur « la même file de sortie ». Entre machines, le `Remove` vient de A et
l'`Add` de B, par deux liens différents. **C'est donc le proxy qui orchestre la
migration**, pas les shards — il possède la connexion, donc il possède la
transition. Un shard décide *qu'*une migration doit avoir lieu ; le proxy la
séquence.

### A.5 Allocation d'IDs, révisée pour le distribué

Deux espaces, deux propriétaires — et c'est plus propre que §10.4 :

| espace                       | alloué par   | pourquoi                                                                                       |
| ---------------------------- | ------------ | ---------------------------------------------------------------------------------------------- |
| **session id** (utilisateur) | le **proxy** | c'est l'identité de la connexion, stable pour toute sa vie, y compris à travers les migrations |
| **channel id**               | le **shard** | un canal appartient à un shard et ne le quitte pas                                             |

Partition statique des bits de poids fort (id de proxy / id de shard) plutôt qu'un
service de bail : zéro coordination. En `u32`, par exemple 12 bits d'espace + 19
bits locaux + 1 bit éphémère. Serré mais suffisant.

⚠️ Rend **Q1 encore plus critique** : ces IDs sont grands et très épars.

### A.6 Ce qui n'est pas gratuit

| point                                                 | nature                                                                                                                                                                                                                                                                                                                             |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Certificat serveur partagé** entre tous les proxies | Le client Mumble indexe ses préférences par shard, et son verdict UDP, sur le `sha1` de la clé publique du serveur. Des certificats différents ⇒ le client croit changer de serveur. Il faut distribuer le même matériel de clé. Opérationnel, mais réel.                                                                          |
| **Stickiness UDP**                                    | Le client envoie son UDP à l'adresse:port de son TCP. Il faut donc que le datagramme atteigne _le proxy qui tient sa connexion TCP_. Le plus simple : une adresse publique par proxy, choix à la connexion (DNS round-robin). Sinon L4 avec hachage cohérent sur l'adresse source. Mumble n'offre pas de mécanisme de redirection. |
| **Mort d'un shard**                                   | Ses connexions ont une vue d'un monde mort. Les proxies détectent le silence du journal et appliquent la politique de Q9, à l'échelle physique.                                                                                                                                                                                    |
| **Le transport interne**                              | Livraison du journal + fanout voix inter-proxy : c'est de l'infrastructure nouvelle, et c'est le vrai coût de cette topologie.                                                                                                                                                                                                     |

### A.7 Réutilisation existante

`tools/mitm-proxy` (P2) est déjà ~80 % du plan de données d'un tel proxy : TLS
terminée des deux côtés, **deux domaines OCB2 indépendants par connexion**,
ré-encryption UDP validée sur corpus réel, corrélation adresse → session. Il avait
été écrit comme oracle de conformité ; il se trouve être la maquette du proxy.

### A.8 Verdict

Techniquement viable, et **le v1 n'a rien à changer pour le permettre** : les
cinq règles de §10.5 sont exactement les invariants dont cette topologie a besoin,
et la seule chose qu'elle ajoute est un transport. La règle à ne jamais violer,
qui est la même à toutes les échelles :

> **Le shard est une autorité de plan de contrôle. Il ne relaie jamais de voix.**

C'est aussi beaucoup de machinerie. Elle reste une réflexion tant qu'aucune mesure
ne la réclame (§27.4 de la spec : pas d'optimisation sans mesure).

---

## 15. Journal des révisions

| date       | révision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-07-27 | **r1.** Première rédaction : diagnostic, modèle à quatre concepts, contrat de flavor, pipeline en quatre termes, ordonnancement, règles de correction, journal vs cache, audio, sharding et migration, complexité, questions ouvertes.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| 2026-07-27 | **r6.** Ajout de **§4.9 — où vivent les `Arc` et les `Mutex`**, et correction d'une formulation trop absolue de r5 : la borne est `Send + 'static` et Mumble Server Runtime ne _demande_ pas `Sync`, ce qui n'interdit pas à un flavor concret de l'être. Trois emplacements, trois propriétaires : le partage métier vit **dans le `ShardLogic` concret** (canal pour un flux d'événements, `Arc<Mutex>` pour du « dernière valeur gagnante », avec le mode de panne du second explicité) ; **`ShardHandle` ne porte aucun état** (un `Arc<Notify>` et un `mpsc::Sender<RuntimeCommand>` — signal et message, jamais pointeur) ; le runtime a des `Arc` mais tous _read-mostly_ (`ArcSwap`) ou par-connexion non contendus. L'énoncé défendable devient : **aucun verrou global, aucun verrou tenu à travers un `.await`, aucun verrou contendu sur le chemin de contrôle**.                                                                                                                                                      |
| 2026-07-27 | **r5.** §4 refondu. `ShardLogic` réduit à **trois méthodes** — `render`, `params`, `observe` : `tick()` et `on_message()`/`type Message` sont retirés, parce que Mumble Server Runtime ne sait ni pourquoi un flavor voudrait un rythme, ni ce qu'un message métier signifie. **Le flavor possède sa propre boîte aux lettres** et la draine dans `render` (§4.2), ce qui règle « alors `ShardLogic` serait partagé ? » sans le rendre `Sync`. **`wake()` est explicité comme la seule interface métier → runtime** (§4.3) : le rythme appartient au flavor, la protection (`MIN_INTERVAL`) au runtime, et **le runtime n'a plus aucun tick**. `ShardControl` supprimé au profit d'une **poignée unique `ShardHandle`** remise à la construction, utilisable dedans comme dehors (§4.4). Ajout de §4.5 (shard piloté par RPC, preuve que trois méthodes suffisent) et §4.6 (**vue opérationnelle** : démarrage, arrivée d'une connexion, régime établi, métriques à exposer). Renumérotation : `Params` → §4.7, `Snapshot` → §4.8. |
| 2026-07-27 | **r4.** §13 étoffé : critère d'existence d'un crate (« une arête à interdire »), **carte des crates proposée** (deux nouveaux purs : `mumble-server-runtime-project` pour le masque/filtrage, `mumble-server-runtime-journal` pour versions et curseurs ; `mumble-server-runtime-control` disparaît ; `mumble-server-runtime-server` devient `mumble-server-runtime-runtime`), décompte des suppressions (~2 500 lignes contre ~800 à écrire), **un oracle par composant** (§13.4) et ce qu'on refuse de découper (§13.5). Ajout de §13.6 : où le risque se déplace, honnêtement.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| 2026-07-27 | **r3.** Ajout de l'**annexe A** (exploratoire, hors périmètre) : scaling horizontal avec shard = machine et étage de proxies façon BungeeCord. Vérifie que les cinq règles de §10.5 suffisent. Résultats notables : le proxy est la promotion de la task de connexion, donc le socket ne change jamais de propriétaire à aucune échelle ; la voix va proxy→proxy pour un coût **O(M) proxies** et non O(R) destinataires ; `Params` en tant que _donnée_ est ce qui rend le modèle distribuable ; les session ids s'allouent depuis le **proxy** et les channel ids depuis le **shard** (révision de §10.4) ; le proxy orchestre la migration puisque la garantie FIFO d'une file unique ne tient plus entre machines. Coûts non gratuits recensés (certificat partagé, stickiness UDP, transport interne).                                                                                                                                                                                                                        |
| 2026-07-27 | **r2.** Renommage World → **Shard**. Le Shard devient un objet runtime de première classe avec un cycle de vie impératif (`create`/`destroy`/`move`/`wake`), et non une énumération dérivée du flavor (§3.2). **Une task par shard** (§3.3), avec parallélisme et isolation de panne. Contrat de flavor éclaté en `ConnectionRouter` (où va une connexion ; **le jeton P8 y atterrit**) et `ShardLogic` (`Send`, délibérément **pas `Sync`**) (§4). **`Snapshot` supprimé** avec justification (§4.5). Possession clarifiée : le shard possède l'_état de contrôle_, la task de connexion possède le _socket_ (§10.1), ce qui rend la migration intra-processus triviale. Protocole de migration détaillé avec la garantie d'ordre FIFO et le traitement des commandes en vol (§10.3). **Nouveau §11 : le plan UDP** — table `ArcSwap<Bindings>`, `ShardRouting` auto-suffisant, hot path sans task de shard, pré-filtrage IP en chemin froid, non-perturbation par la migration, pistes multi-processus. Q9 et Q10 ajoutées.      |
