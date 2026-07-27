# Guide d'implémentation — runtime à shards

> Document autoportant : il se lit linéairement et contient tout ce qu'il faut
> pour écrire le système. `render-scheduling-and-sharding.md` explique *pourquoi*
> on en est arrivé là ; sur le modèle de visibilité, **c'est ce document-ci qui
> fait foi**.
>
> **Statut : étapes 1 à 10 implémentées.** Le cœur pur et la task de shard dans
> `voxloom-shard`, la porte d'entrée dans `voxloom-gateway` (plan de contrôle
> TLS, `ConnectionRouter`, registre multi-shards, migration, plan vocal UDP), et
> un flavor de démonstration dans `tools/voxloom-arena` — un seul exécutable.
> Le pipeline P5–P7 existant n'a pas été retiré : les deux modèles coexistent le
> temps de la bascule, et `voxloom-server` reste sur l'ancien.
> Écarts assumés et points ouverts : §18.
> Révision 3 (2026-07-27) — voir §16.

---

## 0. Le modèle en une page

```
   le métier change
        │  handle.wake()
        ▼
  ┌──────────────────────────────────────────────────────┐
  │ task du shard (réveillée, une par shard)             │
  │                                                      │
  │  1. logic.render(&mut builder)                       │
  │       → la vue PARTAGÉE + les overlays PRIVÉS        │
  │         + la relation AUDIO                          │
  │  2. ops = plan(vue_courante → vue_nouvelle)          │
  │  3. version += 1, ops → journal                      │
  │  4. publier la table de routage audio                │
  │  5. pour chaque connexion :                          │
  │       sa portée a changé ? → replanifier pour elle   │
  │       sinon → filtrer ops, composer avec son overlay │
  │       encoder, pousser, avancer son état engagé      │
  └──────────────────────────────────────────────────────┘
        │
        ▼
  task de connexion : draine sa file, écrit sur le socket
```

Et l'idée qui rend tout ça linéaire : **on ne partage pas les vues, on partage les
changements.** Un delta de trois opérations filtré N fois coûte O(N·|D|), pas
O(N·taille du monde).

---

## 1. Les trois mécanismes

C'est le squelette du document. **Ils sont indépendants**, et vouloir les unifier
est l'erreur qui fait proliférer les cas particuliers.

| # | mécanisme | forme | ce qu'il résout | coût |
|---|---|---|---|---|
| 1 | **vue partagée + portées** | un arbre de portées, une portée par élément, un `ScopeSet` par observateur | parties, équipes, spectateurs, staff — les 95 % | O(W) une fois + O(\|D\|) par connexion |
| 2 | **overlay privé** | quelques éléments visibles par **une seule** connexion | vanish, canal privé, placement par observateur | O(\|overlay\|) |
| 3 | **routage audio** | une relation **orientée** par destinataire | absolument tout l'audio | O(N + arêtes) |

### 1.1 La règle qui dit lequel utiliser

> **Une portée décrit un groupe. Un overlay décrit une exception individuelle.**
> Si tu crées une portée qui n'a qu'un seul observateur, c'est un overlay
> déguisé.

Un rôle — joueur, host, spectateur niveau 1, spectateur niveau 2, staff — est par
nature un **groupe**, même s'il n'a qu'un membre. L'overlay ne sert qu'aux cas où
une *personne* diverge de son propre rôle.

### 1.2 Le seul couplage, et il n'est pas de nous

> **Un destinataire doit voir l'émetteur.**

Le client Mumble **jette l'audio dont il ne connaît pas la session émettrice**
(`ClientUser::get(senderSession)`, `ServerHandler.cpp::handleVoicePacket`). Ce
n'est donc pas un choix de conception : c'est une contrainte de protocole, à
vérifier sur la sortie du flavor — « toute arête `s → r` implique que `r` voit
`s` », que ce soit par la vue partagée ou par son overlay.

Corollaire : **il n'existe pas de « droit de parler » séparé.** Émettre
invisiblement est impossible ; parler quelque part suppose y être visible.

---

## 2. La portée

### 2.1 Définition

Une **portée** est un chemin dans un arbre :

```
/                  la racine : ce que tout le monde voit
/g7                la partie 7
/g7/t2             l'équipe 2 de la partie 7
/g7/t2/p42         un joueur précis (cas extrême, possible)
```

Chaque **élément rendu** occupe une portée. Chaque **connexion** en observe un
petit ensemble. La visibilité tient en une ligne :

> Une connexion voit un élément si **l'une de ses portées d'observation est
> comparable** à celle de l'élément : l'une est un préfixe de l'autre.

```rust
fn comparable(a: Scope, b: Scope) -> bool {
    a.is_prefix_of(b) || b.is_prefix_of(a)
}
```

Les **deux** directions comptent, et c'est ce qui rend la relation utile : un
joueur à `/g7/t2` voit ses ancêtres (`/`, `/g7`) *et* ses descendants ; un
spectateur à `/g7` voit toutes les équipes sans qu'on ait rien ajouté au modèle.
Monter dans l'arbre, c'est voir plus large.

### 2.2 Représentation

Petite, `Copy`, comparable en quelques instructions — c'est indispensable, elle
est consultée une fois par connexion et par tour.

```rust
pub const MAX_DEPTH: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scope {
    /// Segments opaques pour Voxloom : le flavor y met ce qu'il veut
    /// (id de partie, id d'équipe…).
    segments: [u32; MAX_DEPTH],
    depth: u8,
}

impl Scope {
    pub const ROOT: Scope = Scope { segments: [0; MAX_DEPTH], depth: 0 };

    /// La SEULE façon d'en fabriquer une nouvelle : on ne peut jamais élargir.
    pub fn child(self, segment: u32) -> Scope;

    pub fn is_prefix_of(self, other: Scope) -> bool {
        self.depth <= other.depth
            && self.segments[..self.depth as usize] == other.segments[..self.depth as usize]
    }
    pub fn comparable(self, other: Scope) -> bool {
        self.is_prefix_of(other) || other.is_prefix_of(self)
    }
}

/// Ce qu'une connexion observe. Petit, `Copy`, comparable par égalité.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ScopeSet { scopes: [Option<Scope>; 4] }

impl ScopeSet {
    pub fn sees(&self, element: Scope) -> bool {
        self.scopes.iter().flatten().any(|s| element.comparable(*s))
    }
}
```

### 2.3 Le théorème de clôture

C'est ce qui fait que le modèle joue avec nous plutôt que contre nous. Impose une
seule contrainte au rendu :

> **La portée d'un canal enfant étend celle de son parent. La portée d'un
> utilisateur étend celle de son canal.**

Alors la propriété dont tout dépend — *si je vois un élément, je vois ce à quoi il
fait référence* — est **automatique**. Démonstration, avec `c` la portée du canal
et `u` celle de l'utilisateur, `c` préfixe de `u`. Un observateur voit
l'utilisateur via une portée `s` comparable à `u` :

- si `s` est préfixe de `u` : `c` aussi est préfixe de `u`, donc `s` et `c` sont
  deux préfixes du même chemin, donc comparables ✅
- si `u` est préfixe de `s` : `c` préfixe de `u` préfixe de `s` ✅

Aucune vérification à l'exécution, aucun mode d'échec, aucun contre-exemple à
chasser. **Une vue incohérente n'est pas exprimable** si le constructeur du §3
refuse d'élargir.

### 2.4 Portées ≠ arbre des canaux

La règle est « **étend** », pas « égale ». Un canal peut donc contenir des
utilisateurs de portées différentes, filtrés à l'intérieur du canal :

```
canal « Général »       portée /g7      ← visible par toute la partie
  ├─ joueurs rouge      portée /g7/t2   ← visibles par l'équipe 2 seulement
  └─ joueurs bleu       portée /g7/t3   ← visibles par l'équipe 3 seulement
```

Le cas extrême « une portée par joueur » (`/g7/t2/p42`) marche aussi. Ce n'est pas
optimal — l'arbre s'approfondit — mais **ça ne change pas la loi de coût** : le
filtre reste O(|D|) par connexion quel que soit le nombre de portées distinctes.

---

## 3. Ce que le flavor écrit

### 3.1 Trois méthodes

```rust
pub trait ShardLogic: Send + 'static {
    /// Construire la vue partagée, les overlays privés et la relation audio.
    /// Aucun paramètre de spectateur : le PARTAGÉ ne peut pas dépendre de qui
    /// regarde.
    fn render(&mut self, out: &mut ShardBuilder);

    /// Ce que cette connexion observe du partagé.
    fn observation(&mut self, connection: ConnectionId) -> ScopeSet;

    /// Un fait vocal s'est produit. Le flavor décide seul de ce qu'il en fait.
    fn observe(&mut self, event: &VoiceEvent);
}
```

`Send + 'static`, **sans `Sync`** : le runtime n'a jamais besoin de partager la
logique, donc il n'impose aucune synchronisation. Un flavor concret peut se
trouver `Sync` s'il contient un `Arc<Mutex<…>>` — c'est son affaire (§7.1).

`observation()` est appelée N fois par tour : elle **doit** rendre un `ScopeSet`,
c'est-à-dire une valeur `Copy` de deux mots. Si tu es tenté de lui faire rendre
plus gros, la complexité redevient quadratique.

### 3.2 Le constructeur : uniforme, et il ne sait qu'étendre

```rust
impl ShardBuilder {
    // ---- vue partagée ------------------------------------------------------
    pub fn root(&mut self, name: &str) -> ChannelRef;

    /// Un canal fils. `narrow` ne sait qu'étendre la portée du parent.
    pub fn channel(&mut self, parent: ChannelRef, name: &str, narrow: Narrow)
        -> ChannelRef;

    /// Un utilisateur dans un canal. Sa portée étend celle du canal.
    pub fn user(&mut self, channel: ChannelRef, who: Occupant, name: &str,
                narrow: Narrow) -> UserRef;

    pub fn channel_position(&mut self, ch: ChannelRef, position: i32);
    pub fn channel_can_enter(&mut self, ch: ChannelRef, yes: bool);
    pub fn channel_link(&mut self, a: ChannelRef, b: ChannelRef);
    pub fn user_flags(&mut self, u: UserRef, flags: UserFlags);

    // ---- overlays privés ---------------------------------------------------
    /// Des éléments visibles par CETTE connexion seulement.
    pub fn private(&mut self, conn: ConnectionId, f: impl FnOnce(&mut PrivateBuilder));

    // ---- relation audio ----------------------------------------------------
    /// Groupe symétrique : tout le monde du domaine entend tout le monde.
    pub fn audio_domain(&mut self, domain: DomainId, members: &[ConnectionId]);
    /// Exception orientée : il entend, il n'est pas entendu.
    pub fn audio_listen(&mut self, listener: ConnectionId, domain: DomainId);
    /// Arête isolée, si le flavor veut la généralité totale.
    pub fn audio_edge(&mut self, sender: ConnectionId, receiver: ConnectionId);

    /// La liste des connexions attachées, pour les boucles d'overlay.
    pub fn connections(&self) -> &[ConnectionId];
}

pub enum Narrow {
    /// Même portée que le parent.
    Same,
    /// Portée du parent, étendue d'un segment.
    Into(u32),
}

pub enum Occupant {
    /// Un joueur réellement connecté.
    Connection(ConnectionId),
    /// Un utilisateur sans connexion vocale : PNJ, joueur hors Mumble, bot.
    Synthetic(SyntheticId),
}
```

**Pourquoi une vue incohérente n'est pas exprimable** : `channel` exige un parent
et `user` exige un canal ; dans les deux cas la portée se dérive de celle du
parent par `Narrow`, qui ne sait qu'étendre. Il n'y a **pas** de paramètre de
portée libre.

La seule chose qui ne suit pas la hiérarchie est le **lien entre canaux**, d'où
l'unique vérification du modèle :

```rust
// `channel_link` : un lien vers un canal qui n'est pas forcément visible
// n'a pas de sens.
debug_assert!(a.scope.comparable(b.scope));
```

### 3.3 Un rendu qui se lit comme le modèle métier

```rust
fn render(&mut self, out: &mut ShardBuilder) {
    // Drainer sa propre boîte aux lettres : aucun verrou, `&mut self`.
    while let Ok(e) = self.inbox.try_recv() { self.apply(e); }

    let accueil = out.root("Accueil");                                  // /

    for game in &self.games {
        let g = out.channel(accueil, &game.name, Narrow::Into(game.id)); // /g7
        let general = out.channel(g, "Général", Narrow::Same);           // /g7

        for team in &game.teams {
            let t = out.channel(g, &team.name, Narrow::Into(team.id));   // /g7/t2
            let dom = DomainId::team(game.id, team.id);
            let members: Vec<_> = team.players.iter().map(|p| p.conn).collect();

            for p in &team.players {
                out.user(t, Occupant::Connection(p.conn), &p.name, Narrow::Same);
            }
            out.audio_domain(dom, &members);

            // Les spectateurs entendent l'équipe sans être entendus.
            for s in &game.spectators {
                out.audio_listen(s.conn, dom);
            }
        }
        let _ = general;
    }

    // Exceptions individuelles : les admins en vanish.
    for a in &self.vanished {
        out.private(a.conn, |p| p.user_in(a.channel, &a.name, a.session));
        out.audio_listen(a.conn, a.domain);
    }
}

fn observation(&mut self, c: ConnectionId) -> ScopeSet {
    match self.role_of(c) {
        Role::Player { game, team }        => set([team_scope(game, team)]),
        Role::Host { game }                => set([game_scope(game)]),
        Role::Spectator { game, level: 1 } => set([public_scope(game)]),
        Role::Spectator { game, level: 2 } => set([game_scope(game)]),
        Role::Staff { game }               => set([game_scope(game), STAFF]),
        Role::Admin                        => set([Scope::ROOT]),
    }
}
```

Ajouter un rôle, c'est ajouter un bras de `match`. **Le nombre de règles est une
propriété de ton jeu ; ce que l'architecture contrôle, c'est que chaque règle
produise une valeur minuscule.**

### 3.4 La présence : partagée **xor** privée

Où quelqu'un apparaît est décidé par le rendu. Deux possibilités, et **jamais les
deux à la fois** :

- **partagée** : `out.user(canal, …)` → vu par tous ceux dont l'observation
  recoupe sa portée. Le cas courant.
- **privée** : `out.private(observateur, |p| p.user_in(canal, …))` → vu par ce
  seul observateur.

```rust
// Invariant, tenu par une assertion, jamais par une fusion :
debug_assert!(!(shared.contains(session) && any_overlay.contains(session)));
```

Sans cet invariant, la vue partagée dirait « admin dans `/g7/staff` » et l'overlay
« admin dans le canal de A » ; au tour suivant le delta partagé déplacerait
l'admin sans que l'overlay ne se réaffirme, et le client dériverait **en
silence** — la pire classe de bug de ce design. Avec l'invariant, il n'y a rien à
fusionner.

Ce que la présence privée permet, et qui est le cœur du besoin :

- **vanish** : un admin dans n'importe quel vocal, visible de lui seul ;
- **annonce audio** : il « rejoint » virtuellement tous les canaux — une entrée
  d'overlay par observateur, chacune plaçant la **même session** dans *son* canal.
  Coût O(N) éléments, linéaire et visible dans la loi de coût (§13) ;
- **il se voit lui-même** où il est réellement, via son propre overlay.

La règle « dans une vue donnée, une personne n'est qu'à un endroit » devient
structurelle : un overlay appartient à une connexion, et le flavor y met au plus
un placement.

### 3.5 L'audio : une relation, pas un arbre

Les portées ne pilotent **que** le visuel partagé. L'audio est une relation
orientée que le flavor déclare, exactement ton « bitmap de réception par
utilisateur » — un sens, deux sens, ou rien.

| primitive | coût | pour |
|---|---|---|
| `audio_domain(d, membres)` | O(\|membres\|) | le gros : un canal, une équipe |
| `audio_listen(qui, d)` | O(1) | admin, spectateur : entend sans être entendu |
| `audio_edge(s, r)` | O(1) | généralité totale, au coût du flavor |

Deux vérifications sur la sortie :

1. **`r` voit `s`** (§1.2), sinon le paquet serait jeté par le client ;
2 . les domaines ne franchissent pas les frontières de shard — c'est ce qui rend
   la table de routage auto-suffisante (§9.3).

---

## 4. Les identifiants

```rust
pub struct ShardId(u64);
pub struct ConnectionId(u64);   // une connexion vivante : socket, crypto, file
pub struct SessionId(u32);      // un utilisateur visible : ce qui part sur le fil
pub struct ChannelId(u32);      // un canal : ce qui part sur le fil
```

Deux règles absolues sur `SessionId` et `ChannelId` :

- **Jamais réutilisés.** Un identifiant retiré est mort pour toujours : le client
  Mumble attache des préférences locales aux identifiants de canaux.
- **`ChannelId(0)` est la racine**, et elle appartient au runtime, pas à un
  shard. Les shards sont des sous-arbres sous elle.

**Pourquoi `SessionId` ≠ `ConnectionId`** : parce que tout le monde n'est pas
connecté. La relation est `SessionId ⊇ ConnectionId` — un PNJ, un joueur hors
Mumble, un bot ont une session mais ni socket, ni clé, ni curseur. C'est le rôle
d'`Occupant::Synthetic`.

---

## 5. Le diff : la portée fait partie de l'identité

**La clé de comparaison du diff est `(élément, portée)`, pas `élément`.** Ça coûte
zéro ligne et ça supprime toute une famille de cas particuliers.

Quand un joueur change d'équipe, sa portée passe de `/g7/t3` à `/g7/t2`. Ce n'est
pas « un champ qui change », c'est *l'entrée `(B, /g7/t3)` qui disparaît et
l'entrée `(B, /g7/t2)` qui apparaît*. Le diff ordinaire produit donc tout seul :

```
RemoveUser(B)              [portée /g7/t3]
AddUser(B, canal Équipe2)  [portée /g7/t2]
```

Et le filtre reste **un seul test**, sans branche :

| connexion | observe | reçoit |
|---|---|---|
| coéquipier resté en t3 | `{/g7/t3}` | le `Remove` seul → B disparaît ✅ |
| joueur de t2 | `{/g7/t2}` | le `Add` seul → B apparaît ✅ |
| spectateur | `{/g7}` | les **deux** → réglé par `collapse` (§6.3) |
| joueur d'une autre partie | `{/g8}` | rien ✅ |

> **Propriété qui tombe toute seule :** un déplacement *à l'intérieur* d'une
> portée reste un `MoveUser` ; un déplacement *entre* portées devient un départ et
> une arrivée. Ce qui est sémantiquement exact.

### 5.1 Les opérations de plan

Des mutations de vue, pas des messages Mumble.

```rust
pub enum PlanOp {
    CreateChannel(Channel),
    UpdateChannel(ChannelPatch),
    AddUser(User),
    MoveUser { session: SessionId, channel: ChannelId },
    UpdateUser(UserPatch),
    RemoveUser(SessionId),
    RemoveChannel(ChannelId),
}

struct PlannedOp {
    op: PlanOp,
    /// Pour un Remove, c'est la portée dans la vue PRÉCÉDENTE : l'élément
    /// n'existe plus dans la nouvelle, on ne peut pas l'y relire.
    scope: Scope,
}
```

L'ordre global, qui porte les invariants §20 :

```
P1  CreateChannel      (parents avant enfants)
P2  UpdateChannel
P3  AddUser
P4  MoveUser           (avant toute suppression : aucun canal occupé n'est retiré)
P5  UpdateUser
P6  RemoveUser
P7  RemoveChannel      (enfants avant parents)
```

> **Aucune opération audio dans le plan.** L'audio vient d'une table séparée et
> son ordre est garanti autrement (§9.5). Deux variantes de moins qu'au modèle
> précédent.

---

## 6. La composition par connexion

C'est le cœur du runtime. Trois étapes, une quinzaine de lignes.

```rust
let shared  = filter(&journal.replay(conn.cursor, head), conn.see);
let private = plan_elements(&conn.overlay_sent, &overlay_new);
let mut ops = splice(shared, private);
collapse(&mut ops);
```

### 6.1 `filter` — un test de préfixe

```rust
fn filter(ops: &[PlannedOp], see: ScopeSet) -> Vec<PlanOp> {
    ops.iter().filter(|p| see.sees(p.scope)).map(|p| p.op.clone()).collect()
}
```

**Le filtrage préserve l'ordre**, gratuitement : toutes les règles
d'ordonnancement sont des contraintes « X avant Y », et retirer des éléments d'une
séquence n'en viole aucune. Un plan valide filtré reste valide — **il n'y a rien
à replanifier**. C'est la clôture (§2.3) qui fait le vrai travail : elle garantit
qu'aucune référence ne devient pendante.

### 6.2 `splice` — insérer l'overlay **dans** les phases

L'overlay ne s'ajoute pas à la fin. Le contre-exemple est immédiat :

> Le partagé supprime le canal C. L'overlay y avait placé l'admin. Si les
> opérations privées viennent en dernier, `RemoveChannel(C)` part **avant** le
> retrait de l'admin → on supprime un canal occupé. **Invariant 8 violé.**

La bonne insertion suit les phases existantes :

```
P1‥P5   ajouts partagés, filtrés
        ├─ ajouts d'overlay      ← après : ils peuvent viser un canal tout neuf
        └─ retraits d'overlay    ← avant : ils libèrent un canal qui va mourir
P6‥P7   retraits partagés, filtrés
```

### 6.3 `collapse` — une passe, trois emplois

```rust
/// Si un élément a un Add ET un Remove qui survivent, on jette le Remove.
/// Le Add porte l'état complet, et `UserState`/`ChannelState` ont une
/// sémantique de FUSION côté client : le réémettre en entier met simplement
/// l'élément à jour.
fn collapse(ops: &mut Vec<PlanOp>) {
    let added: HashSet<ElementId> = ops.iter().filter_map(PlanOp::added).collect();
    ops.retain(|op| !matches!(op.removed(), Some(id) if added.contains(&id)));
}
```

Parce que dans tous les cas la bonne réponse est la même : **un élément qui a un
`Add` quelque part existe encore, donc le `Remove` est faux.**

| situation | ops produites | après `collapse` |
|---|---|---|
| l'élément change de portée (§5) | Remove(ancienne) + Add(nouvelle) | Add seul ✅ |
| vanish → unvanish | Remove(overlay) + Add(partagé) | Add seul, fusion → il change de canal ✅ |
| unvanish → vanish | Remove(partagé) + Add(overlay) | Add seul ✅ |
| retiré de l'overlay, absent du partagé | Remove seul | Remove conservé ✅ |
| disparaît partout | Remove seul | Remove conservé ✅ |

Trois problèmes qu'on croyait distincts, une seule fonction : c'est le signe
qu'elle est au bon niveau.

### 6.4 Ce qu'il ne faut **pas** faire

`diff(overlay + partagé, engagé)` est la formulation honnête et toujours
correcte — et elle coûte **O(V) par connexion**, donc O(N·W) au total. C'est
exactement le quadratique qu'on fuit (198 ms à 500 connexions dans le modèle
actuel). La composition, elle, coûte `O(|D|) + O(|overlay|) + O(|ops|)`.

### 6.5 L'état engagé est un **triplet**

```rust
struct AttachedConnection {
    id: ConnectionId,
    session: SessionId,
    cursor: u64,             // où elle en est du PARTAGÉ
    see: ScopeSet,           // ce qu'elle observe du partagé
    overlay_sent: Overlay,   // ce qu'elle a reçu de PRIVÉ
    queue: mpsc::Sender<OutboundItem>,
    shared_cursor: Arc<AtomicU64>,   // lu par le plan UDP (§9.5)
}
```

Les trois avancent **ensemble**, au même endroit que tout le reste : quand la file
a tout accepté. C'est ce qui permet à une connexion en retard de rattraper
naturellement — le partagé se rejoue depuis le journal, le privé se recalcule
depuis `overlay_sent`. **L'overlay n'a pas besoin d'être journalisé.**

### 6.6 La vérification de l'overlay

Une seule, au moment où le flavor le construit :

> **Un overlay ne peut référencer qu'un élément présent dans la vue partagée
> *après* transition, et visible par cette connexion.**

Un `lookup` par élément d'overlay. Ça interdit d'un coup : placer quelqu'un dans
un canal qui va disparaître, dans un canal que cette connexion ne voit pas, ou
référencer une session inexistante.

---

## 7. Qui possède quoi

Règle unique : **chaque donnée mutable a exactement une task propriétaire, et on
communique en déplaçant des valeurs.**

| task | il y en a | possède | ne fait jamais |
|---|---|---|---|
| **runtime** | 1 | registre des shards et des connexions, bindings UDP, allocateurs d'identifiants | de calcul long |
| **shard** | 1 par shard | l'état métier, la vue courante, le journal, l'état de contrôle des connexions | **awaiter une I/O** |
| **connexion** | 1 par connexion | socket TLS, état OCB2, boucle lecture/écriture | tenir un verrou pendant un `.await` |
| **plan UDP** | 1 ou quelques-unes | la socket UDP | toucher une task de shard |

```
                       ┌──────────────┐
                       │   runtime    │ ← commandes (create/destroy/move/attach)
                       └───┬──────┬───┘
                 ArcSwap   │      │  mpsc
             (bindings UDP)│      │
             ┌─────────────┘      └──────────────┐
             ▼                                   ▼
      ┌─────────────┐                     ┌─────────────┐
      │  shard A    │                     │  shard B    │
      └──────┬──────┘                     └──────┬──────┘
             │ mpsc (octets)                     │
   ┌─────────┼─────────┐                         │
   ▼         ▼         ▼                         ▼
 conn 1    conn 2    conn 3                    conn 4
   ▲         ▲         ▲                         ▲
   └─────────┴────┬────┴─────────────────────────┘
                  │ mpsc (octets de voix)
           ┌──────┴──────┐
           │  plan UDP   │ ← lit ArcSwap<Bindings> et ArcSwap<ShardRouting>
           └─────────────┘
```

### 7.1 Où vivent les `Arc` et les `Mutex`

La proposition ne prétend pas « aucun `Arc` nulle part ». Elle prétend :

> **Aucun verrou global. Aucun verrou tenu à travers un `.await`. Aucun verrou
> contendu sur le chemin de contrôle.**

**Le partage métier vit dans le `ShardLogic` concret**, jamais ailleurs :

| forme | quand | conséquence |
|---|---|---|
| `mpsc::Receiver<Msg>` dans la logique | **flux d'événements** | aucun verrou ; `try_recv()` ne bloque jamais ; backpressure naturelle |
| `watch::Receiver<Arc<State>>` | **dernière valeur gagnante** (positions à 20 Hz) | pas de file de valeurs périmées ; l'émetteur construit hors verrou |

À éviter : `Arc<Mutex<World>>` où `render` verrouille et construit la vue sous le
verrou. La section critique contiendrait du code métier de durée arbitraire, et un
écrivain extérieur qui ferait de l'I/O sous le verrou **bloquerait la task du
shard**.

**Les `Arc` du runtime**, tous *read-mostly* ou non contendus :

| structure | partagée entre | pourquoi c'est sans risque |
|---|---|---|
| `ArcSwap<Bindings>` | runtime → plan UDP | copy-on-write, écrite rarement, lue sans verrou |
| `ArcSwap<ShardRouting>` | task de shard → plan UDP | idem ; le contenu est une **valeur**, donc transmissible par réseau plus tard |
| `Arc<Mutex<CryptState>>` | plan UDP ↔ task de connexion | **par connexion**, jamais contendu, et toujours sur la même machine |
| `Arc<AtomicU64>` (curseur) | connexion → plan UDP | atomique |

### 7.2 La règle de la frontière réseau

À suivre dès le v1 : elle ne coûte rien maintenant et décide de tout plus tard.

> **Toute communication entre deux tasks passe par un canal de valeurs possédées.
> Un `Arc<Mutex<T>>` partagé entre deux composants est une frontière soudée : il
> ne franchira jamais une limite de processus.**

---

## 8. Les structures

### 8.1 Le journal

```rust
struct Journal {
    tail: u64,
    head: u64,
    entries: VecDeque<Vec<PlannedOp>>,
}
const DEPTH: usize = 256;
```

- `push(ops)` incrémente `head`, évince du début au-delà de `DEPTH`.
- `replay(from, to)` concatène les deltas `from+1 ..= to`.
- **Curseur sous `tail` ⇒ on ferme la connexion** (ADR-009). Elle repartira de
  zéro. Une connexion qui a raté 256 mises à jour est de toute façon en train de
  mourir : sa file fait 1024 messages.

> **Il n'y a aucun mécanisme d'instantané à écrire.** Un nouvel arrivant reçoit
> `plan(vue_vide → vue_courante)` — le planificateur ordinaire. Un départ, c'est
> l'inverse. Une migration, c'est les deux.

### 8.2 L'overlay

```rust
struct Overlay {
    channels: BTreeMap<ChannelId, Channel>,   // canaux privés, rares
    users: BTreeMap<SessionId, User>,         // placements par observateur
}
```

`plan_elements(ancien, nouveau)` est le planificateur **sans les vérifications de
vue complète** (pas de racine à exiger) : un overlay n'est pas une vue autonome.
Il applique les mêmes règles d'ordre internes — canaux avant utilisateurs,
utilisateurs retirés avant canaux.

### 8.3 La table de routage

```rust
struct ShardRouting {
    /// Le calcul pur existant : qui peut entendre qui, orienté.
    audio: AudioRoutingSnapshot,
    /// Parallèle à l'index de session.
    delivery: Vec<Delivery>,
}

struct Delivery {
    session: SessionId,
    since: u64,                  // version du shard à laquelle il est apparu
    /// L'état VIVANT du transport : adresse UDP et drapeau de mode en
    /// atomiques, file de sortie, état OCB2. Surtout PAS une photographie :
    /// une bascule UDP↔tunnel ne doit jamais forcer une recompilation.
    sink: Arc<ConnectionSink>,
}
```

`since` est **par participant**, pas par paire : c'est ce qui garde la table en
O(N) et non O(N²).

**Elle est remplacée en entier, jamais diffusée.** C'est délibéré : le plan UDP la
lit par `ArcSwap::load()`, une lecture atomique d'une valeur **immuable**. La
muter sur place voudrait dire prendre un verrou sur le chemin de la voix — ce
qu'ADR-005 interdit.

Mais on ne la **recompile** que si nécessaire :

```rust
// `compile` est quadratique par construction (13,5 µs à 128 participants).
if delta.touches_membership() || any_scope_changed || audio_edges_changed {
    self.routing.store(Arc::new(self.compile_routing()));
}
// sinon : on garde l'Arc précédent, coût nul.
```

Un renommage de canal, un changement de position, une horloge à 10 Hz : rien de
tout ça ne touche au routage.

---

## 9. Les algorithmes

### 9.1 La boucle d'un shard

```rust
async fn shard_task(mut shard: Shard) {
    let mut prochaine_publication = Instant::now();
    loop {
        attendre_un_premier_evenement().await;
        vider_la_mailbox_sans_attendre(&mut shard);

        if Instant::now() < prochaine_publication {
            // Les commandes continuent d'être traitées pendant cette attente.
            absorber_jusqu_a(prochaine_publication, &mut shard).await;
        }

        shard.reconcile();
        prochaine_publication = Instant::now() + MIN_INTERVAL;
    }
}
```

**Il n'y a aucun tick dans le runtime.** Voxloom ne sait pas pourquoi un flavor
voudrait un rythme : un flavor qui en veut un lance son propre
`tokio::interval` et appelle `wake()`. `MIN_INTERVAL` (50 ms pour commencer) est
le seul réglage, et c'est une **protection**, pas une politique. Il borne les
publications à 20 Hz, jamais la consommation de la mailbox : `observe()` reste
immédiat et toutes les commandes reçues dans la fenêtre sont coalescées dans le
même `reconcile()`.

```rust
enum ShardCommand {
    Attach(AttachedConnection),
    Detach(ConnectionId, DetachReason),
    /// La file d'une connexion s'est vidée : on réessaie POUR ELLE SEULE, en O(1).
    Drained(ConnectionId),
    Event(VoiceEvent),
}
```

### 9.2 `reconcile()`

```rust
fn reconcile(&mut self) {
    // 1. Le flavor construit tout : partagé, overlays, relation audio.
    let mut b = ShardBuilder::new(&mut self.ids, &self.connections);
    self.logic.render(&mut b);
    let (view, overlays, audio) = b.finish();

    // 2. Planifier. Clé = (élément, portée), donc un changement de portée
    //    devient un Remove + un Add, gratuitement.
    let ops = plan(&self.view, &view);

    // 3. Qui a bougé dans l'arbre des portées ?
    let moved: Vec<_> = self.connections.values()
        .filter(|c| self.logic.observation(c.id) != c.see)
        .map(|c| c.id).collect();

    if ops.is_empty() && moved.is_empty() && overlays == self.overlays_sent_all() {
        return;
    }

    // 4. Avancer la version et journaliser.
    self.version += 1;
    self.journal.push(ops);
    let previous = std::mem::replace(&mut self.view, view);

    // 5. Publier le routage AVANT de pousser les vues : une route retirée
    //    disparaît immédiatement (couper trop tôt est toujours sûr), une route
    //    ajoutée est inerte tant que le curseur ne l'a pas rattrapée (§9.5).
    self.maybe_recompile_routing(&audio);

    // 6. Pousser.
    for conn in self.connections.values_mut() {
        let overlay = overlays.get(&conn.id).cloned().unwrap_or_default();
        if moved.contains(&conn.id) {
            replan(conn, &previous, &self.view, self.logic.observation(conn.id), overlay);
        } else {
            push(conn, &self.journal, self.version, &overlay);
        }
    }
}
```

### 9.3 Chemin rapide : `push()`

```rust
fn push(conn: &mut AttachedConnection, journal: &Journal, head: u64,
        overlay: &Overlay) {

    if conn.cursor < journal.tail {
        return close(conn, "trop en retard, reconnexion nécessaire");
    }

    let shared  = filter(&journal.replay(conn.cursor, head), conn.see);
    let private = plan_elements(&conn.overlay_sent, overlay);
    let mut ops = splice(shared, private);
    collapse(&mut ops);

    if ops.is_empty() {
        // Rien de visible n'a changé pour elle. On avance quand même son
        // curseur, sinon elle traînerait jusqu'à tomber hors du journal.
        conn.set_cursor(head);
        return;
    }

    // Tout ou rien : réserver toutes les places AVANT d'écrire le premier
    // message. Une transition à moitié livrée laisse le client dans un état
    // que personne ne sait décrire.
    match conn.queue.try_send_all(ops.iter().map(encode)) {
        Ok(()) => {
            conn.set_cursor(head);
            conn.overlay_sent = overlay.clone();   // les deux ensemble
        }
        Err(Congested) => { /* rien n'a bougé ; `Drained` réessaiera */ }
        Err(TooLarge)  => close(conn, "transition plus grosse que sa file"),
    }
}
```

### 9.4 Chemin lent : la connexion a changé de portée

Elle doit apprendre tout son nouveau sous-arbre et oublier l'ancien. Aucun delta
ne peut raccourcir ça : l'information qui lui manque n'est dans **aucun**
changement, elle était déjà là.

```rust
fn replan(conn: &mut AttachedConnection, before: &ShardView, after: &ShardView,
          new_see: ScopeSet, overlay: &Overlay) {
    let mut ops = plan(
        &compose(restrict(before, conn.see), &conn.overlay_sent),
        &compose(restrict(after,  new_see),  overlay),
    );
    collapse(&mut ops);
    conn.see = new_see;
    send(conn, ops);            // même politique tout-ou-rien
}
```

Trois choses :

- C'est le planificateur **ordinaire** sur deux vraies vues : tous les invariants
  §20 sont tenus par construction, self compris.
- Ce n'est **pas** un `detach`+`attach` par la vue vide : ce qui est commun aux
  deux portées (les canaux publics) n'est pas touché → **pas de clignotement**
  de son propre arbre, propriété que le checkpoint P6 validait explicitement.
- Coût O(W), **pour cette connexion seule**, sur un événement métier rare.

> **Le chemin lent, c'est l'ancien modèle par connexion.** Il n'a pas disparu :
> il est devenu l'exception au lieu de la règle. C'est toute la thèse du design.

### 9.5 Le gating audio : l'ordre sans protocole

- **On coupe avant de faire disparaître** : gratuit, la table est publiée avant
  les vues et une route retirée est simplement absente. Couper trop tôt est
  toujours sûr — on entend moins que son dû, jamais plus.
- **On branche après avoir montré** : une comparaison dans le plan UDP.

```rust
if receiver.cursor.load(Relaxed) >= sender.since {
    deliver();
}
```

Un chargement atomique, une comparaison. Volontairement conservateur : un
destinataire en retard perd aussi de l'audio d'émetteurs qu'il voyait déjà, au
pire quelques centaines de millisecondes de silence pour un client déjà en
difficulté.

### 9.6 Attacher, détacher, migrer

Les trois sont le même mécanisme.

**Attacher** `c` au shard `S` : `see = S.logic.observation(c)` ;
`ops = plan(vue_vide → compose(restrict(S.view, see), overlay))` ; pousser ;
`cursor = S.version` ; l'ajouter à `S.connections` ; recompiler le routage
(`since = S.version`) ; `observe(Connected)`.

⚠️ **L'overlay doit être appliqué avant `ServerSync`** : le client cherche sa
propre session dès qu'il la reçoit, et un admin vanished n'a **aucune** présence
partagée.

**Détacher** `c` : le retirer de `S.connections` et recompiler le routage (il
cesse immédiatement d'entendre et d'être entendu) ; pousser
`plan(sa vue → vue_vide)` ; `observe(Disconnected)`.

**Migrer** de `A` vers `B` : détacher de `A`, attacher à `B`. **Rien d'autre.**
L'ordre est garanti sans protocole : `A` pousse son retrait dans la file de `c`
avant que le runtime ne transmette à `B`, `B` pousse son ajout après, et la file
est FIFO.

**Le socket ne bouge jamais.** Il appartient à la task de connexion. Seule change
la mailbox à laquelle elle envoie ses événements (`ArcSwap<Sender>` mis à jour par
`B`). Un message qui atterrit quand même chez `A` est **jeté avec un log** : une
commande entrante est consultative.

### 9.7 Le chemin d'un paquet UDP

**Aucune task de shard n'y participe.**

```rust
type Bindings = HashMap<SocketAddr, Binding>;
struct Binding {
    session: SessionId,
    crypt: Arc<Mutex<CryptState>>,
    routing: Arc<ArcSwap<ShardRouting>>,   // la table du shard de cette connexion
}
```

```
1. recv_from(addr)
2. bindings.load().get(&addr)          → 1 hash, sans verrou
                                          absent ⇒ chemin froid
3. déchiffrer avec binding.crypt       → mutex de CETTE connexion, jamais contendu
4. binding.routing.load()              → 1 atomique
5. audio.receivers(sender, target)     → tranche empruntée, zéro allocation
6. pour chaque destinataire, si §9.5 passe : chiffrer avec SA clé,
   puis send_to (UDP) ou pousser dans sa file (tunnel TCP)
```

**Chemin froid, adresse inconnue** : candidats = connexions dont l'IP hôte TCP est
la même ; tenter `checkDecrypt` sur chacune ; lier au premier succès. C'est sûr
parce qu'un déchiffrement OCB2 en échec est **sans effet de bord** (l'IV est
restauré, rien n'est écrit dans l'historique anti-rejeu). L'index *IP hôte →
connexions* est **runtime-global**, pas par shard : on ne sait pas encore de quel
shard vient l'émetteur.

> **Une migration ne perturbe pas l'UDP.** L'adresse, la clé et la session ne
> changent pas ; seul le champ `routing` du `Binding` est réécrit.

---

## 10. Les poignées

```rust
/// Liée à UN shard. Ne contient AUCUN état : un signal et un canal de commandes.
pub struct ShardHandle {
    shard: ShardId,
    wake: Arc<Notify>,
    cmd: mpsc::Sender<RuntimeCommand>,
}

impl ShardHandle {
    /// « Mon état a changé, re-rends-moi quand tu peux. »
    /// LA seule interface métier → runtime, et elle ne transporte aucune donnée.
    pub fn wake(&self);
    pub fn move_connection(&self, c: ConnectionId, to: ShardId);
    pub fn close_connection(&self, c: ConnectionId, reason: &str);
    pub fn runtime(&self) -> &RuntimeHandle;
}

impl RuntimeHandle {
    /// L'œuf et la poule sont résolus par une closure.
    pub fn create_shard<L: ShardLogic>(
        &self, build: impl FnOnce(ShardHandle) -> L) -> ShardHandle;
    pub fn destroy_shard(&self, shard: ShardId, reason: &str);
    pub fn move_connection(&self, c: ConnectionId, to: ShardId);
}
```

```rust
let game = runtime.create_shard(|h| MinecraftGame::new(game_id, h, rx));
```

### 10.1 Le routeur de connexions

Une connexion qui vient de s'authentifier n'appartient à aucun shard : la décision
ne peut pas venir d'un shard.

```rust
pub trait ConnectionRouter: Send + Sync + 'static {
    /// Sur la task de la connexion, jamais sur une task de shard. Peut awaiter :
    /// valider un jeton, interroger un service.
    async fn route(&self, identity: &ConnectionIdentity) -> RouteDecision;
}

pub struct ConnectionIdentity {
    pub name: String,
    pub certificate_hash: Option<String>,
    pub credential: Option<String>,   // le champ `Authenticate.password`
}
pub enum RouteDecision { Attach(ShardId), Reject(String) }
```

**C'est ici que se branche l'authentification par jeton.**

### 10.2 Vue opérationnelle

**Démarrage** : construire le runtime (sockets, allocateurs, bindings) →
installer le `ConnectionRouter` → créer les shards initiaux (ou aucun, si le
routeur les crée à la demande) → `serve()`.

**Arrivée** : TLS → `Version` → `Authenticate` → `router.route().await` →
`Attach(shard)` ou `Reject`. La task de connexion existe déjà ; le shard apprend
son existence à l'attachement.

**En régime** : les tasks de shard **dorment** sur leur `Notify`. Un `wake()` ou un
événement vocal en réveille une ; elle rend, diffuse, se rendort. Les tasks de
connexion drainent leurs files. Le plan UDP tourne sans jamais toucher un shard.

**À exposer à un opérateur** : par shard — version, connexions, durée du rendu,
taille des deltas, fréquence des réveils, **retard maximal d'un curseur** (le
meilleur indicateur de santé) ; global — bindings UDP, paquets/s, connexions
refusées.

---

## 11. Les règles à ne jamais violer

1. **Une task de shard n'attend jamais d'I/O.**
2. **Un verrou n'est jamais tenu à travers un `.await`.**
3. **Un identifiant retiré ne revient jamais.**
4. **Un élément est partagé xor privé** (§3.4).
5. **On publie la table de routage avant de pousser les vues.**
6. **L'état engagé n'avance que si la file a tout accepté**, et les trois
   composantes avancent ensemble.
7. **Une vue invalide ne remplace jamais la vue courante** : on loggue, on garde
   l'ancienne, on ne ferme pas la connexion.

---

## 12. L'oracle

```
état du client simulé  ==  restrict(vue_partagée[cursor], see)  ⊕  overlay_sent
```

À vérifier après **chaque** envoi, dans un proptest qui entrelace au hasard les
**quatre** classes de changement. C'est le point critique : un générateur qui n'en
couvre que deux laisse passer les bugs les plus importants.

```
1. contenu partagé                  (nom, position d'un canal)
2. portée d'un ÉLÉMENT              (un joueur change d'équipe)
3. portée d'une CONNEXION           (le joueur observé change d'équipe)
4. overlay d'une connexion          (vanish / unvanish)
   + les croisements méchants :
     · le partagé retire un canal que l'overlay occupe   → justifie `splice`
     · un élément traverse partagé ↔ privé               → justifie `collapse`
   + congestion de file, migrations de shard
```

**Tests de mutation**, pour prouver que le proptest est réel :

- retirer la portée de la clé du diff (§5) → doit échouer sur la classe 2 ;
- remplacer `splice` par un `append` → doit échouer sur le premier croisement ;
- retirer `collapse` → doit échouer sur le second.

Si l'un d'eux passe quand même, c'est le générateur qui est trop pauvre, pas le
code qui est correct.

---

## 13. La loi de coût

```
par tour et par shard =
      O(W)                                   ← le rendu partagé, une fois
    + O(N × |D|)                             ← le filtre, une passe par connexion
    + Σ_c |overlay_c|                        ← les exceptions individuelles
    + Σ_{c dont l'observation a changé} O(W) ← les replans
    + O(N + arêtes)  si le routage a changé
```

Ce qui **n'y est pas** : le nombre de rôles distincts. On ne mémoïse pas par
classe, on filtre par connexion — ça coûte pareil que les connexions aient 2 rôles
ou 200. **Ce qui coûte, c'est la taille de chaque écart, pas leur nombre.**

En régime établi `|D|` vaut quelques opérations et les overlays sont vides :
**O(W + N)**, linéaire. Les shards étant indépendants et sur des tasks séparées,
K shards se répartissent sur K cœurs.

Le seul pic à connaître : **beaucoup de gens changent de rôle en même temps**
(début de manche). Chacun déclenche un `replan` en O(W), donc O(N·W) d'un coup.
La coalescence `MIN_INTERVAL` fait qu'ils sont tous replanifiés **dans le même
tour**, et sharder par partie borne le pic à la taille d'une partie.

---

## 14. Ordre de construction

| # | à écrire | fini quand |
|---|---|---|
| 1 | `Scope`, `ScopeSet` : `child`, `is_prefix_of`, `comparable`, `sees`. **Pur, ~80 lignes.** | propriétés : `comparable` réflexive et symétrique ; `child` ne rend jamais un préfixe strict de son parent |
| 2 | `ShardBuilder` + types de vue + `finish()`. **Pur.** | propriété : **toute** vue produite satisfait la clôture — impossible à faire échouer, c'est le but |
| 3 | `diff` + `plan` avec la clé `(élément, portée)` ; retirer les ops audio ; faire porter la portée par l'op | le proptest existant à 4000 graines repasse au vert **et** un changement de portée produit bien Remove + Add |
| 4 | `filter`, `splice`, `collapse`. **Pur, ~50 lignes.** | les propriétés du §12 avec un générateur couvrant les quatre classes |
| 5 | `Journal`. **Pur, sans vocabulaire de vue.** | proptest : rejouer par morceaux == d'un coup ; tomber sous `tail` est détecté |
| 6 | Une task de shard, **une** connexion : boucle, `reconcile`, `push`, file bornée | le client simulé reçoit l'arbre, aucune violation §20 |
| 7 | N connexions à portées différentes, `replan`, overlays | deux clients voient des arbres différents ; l'un change de portée et converge **sans clignotement** ; un vanish apparaît chez un seul |
| 8 | Plan UDP : `Bindings`, `ShardRouting`, chemin froid, gating | deux vrais clients s'entendent, en UDP et en repli tunnel ; un admin en `audio_listen` entend sans être entendu |
| 9 | Plusieurs shards : `RuntimeHandle`, `ConnectionRouter`, attacher/détacher | deux clients dans deux shards ne se voient ni ne s'entendent |
| 10 | Migration, `ShardHandle`, `wake`, flavor de référence | scénario complet rejoué par un binaire de composition |

Les étapes **1 à 5 sont entièrement pures** — ni tokio, ni socket, ni horloge.
C'est la moitié du système, testable sans rien lancer.

---

## 15. Pièges

**Propres à ce design**

- **Ne jamais mettre de contenu changeant dans une identité.** Une horloge dans un
  *nom* de canal, c'est un champ. Dans l'*identité* du canal, c'est un canal
  détruit et recréé dix fois par seconde, avec des identifiants brûlés.
- **La portée d'une suppression vient de la vue d'avant.** Fais-la porter par
  l'opération à la planification.
- **Avance le curseur même quand le filtrage ne laisse rien passer**, sinon une
  connexion qui ne voit rien changer tombe hors du journal et tu la fermes sans
  raison.
- **`try_send_all` doit être tout-ou-rien.**
- **`observation()` doit rester `Copy` et minuscule.** Appelée N fois par tour.
- **L'overlay se `splice`, il ne s'`append` pas** (§6.2).
- **`Delivery` ne photographie pas le transport** (§8.3), sinon chaque bascule
  UDP↔tunnel force une recompilation du routage.
- **Le générateur du proptest doit couvrir les quatre classes** (§12). C'est la
  seule chose qui sépare un test qui prouve d'un test qui rassure.

**Hérités, déjà payés une fois**

- **TLS épinglé en 1.2.** Le client Mumble macOS (Qt/OpenSSL) segfault à la fin
  d'un handshake TLS 1.3, avant tout message Mumble.
- **Se connecter à `127.0.0.1`, pas `localhost`** (IPv6).
- **La réponse au `Ping` TCP doit reporter les compteurs OCB2**
  (`good`/`late`/`lost`/`resync`) : un `good` à zéro fait basculer le vrai client
  en tunnel TCP définitif au bout de 20 s, en silence.
- **Séparation vérificateur (R2)** : jamais `voxloom-testkit/` dans le même commit
  qu'un crate de production.
- **CI sous `RUSTFLAGS="-D warnings"`** : les modules de test ont besoin de
  `#![allow(clippy::expect_used)]`.

---

## 16. Journal des révisions

| rév. | changement |
|---|---|
| **r3** | **Trois mécanismes explicitement séparés** (§1) : vue partagée + portées / overlay privé / relation audio, avec la règle « portée = groupe, overlay = exception individuelle ». La portée d'un utilisateur **étend** celle de son canal au lieu de l'égaler (§2.4) : un canal peut contenir plusieurs ensembles filtrés, jusqu'à une portée par joueur — le théorème de clôture est reprouvé. L'audio quitte les portées et devient une **relation orientée** (`audio_domain` / `audio_listen` / `audio_edge`), donc asymétrique par nature ; `Observation` se réduit à `see`. **Overlay réintégré** comme placement par observateur (vanish, annonce audio), avec l'invariant **partagé xor privé** et sa composition détaillée : `splice` dans les phases (§6.2) et `collapse` généralisé (§6.3). L'état engagé devient un **triplet** `(cursor, see, overlay_sent)` avançant atomiquement (§6.5). Routage recompilé **conditionnellement** et `Delivery` pointant vers le transport vivant (§8.3). Loi de coût explicitée (§13) : aucun terme en nombre de rôles. Oracle à **quatre** classes de changement + trois tests de mutation (§12). |
| **r2** | La visibilité passe d'une étiquette posée à côté des éléments à une **portée** (position dans un arbre) ; la clôture devient un théorème au lieu d'une validation ; `render` construit au lieu de décrire. |
| **r1** | Première rédaction : journal + curseur, filtrage, chemin UDP, ordre de construction. |

---

## 17. Ce qui reste à trancher

1. ~~**Le client Mumble accepte-t-il des identifiants de canaux grands et
   épars ?**~~ **Oui, tranché** : le client indexe ses canaux dans un
   `QHash< unsigned int, Channel * >` et rien n'y suppose la densité ni un
   maximum. REF `mumble/src/Channel.h:82` (`c_qhChannels`). L'allocateur peut
   donc distribuer l'espace des `u32` sans le tasser, ce qui est ce qui rend
   « un identifiant retiré ne revient jamais » tenable pour tout un runtime.
2. **Écris `observation()` pour ton UHC réel** — joueur, host, spectateur ×2,
   staff, admin. Si un rôle ne rentre ni dans une portée ni dans un overlay borné,
   il vaut mieux le savoir avant cinq mille lignes.
3. **`MAX_DEPTH`, `DEPTH` du journal, `MIN_INTERVAL`** : 4, 256 et 50 ms sont des
   points de départ, pas des vérités.
4. **Que devient une connexion quand son shard est détruit ?** Repli vers un shard
   d'accueil, ou fermeture. Politique, donc au flavor — mais le runtime doit
   offrir un défaut sûr.
5. **Utilisateurs synthétiques** : `Occupant::Synthetic` leur ouvre la porte, mais
   il reste à décider comment leur session est allouée et comment l'audio les
   référence (ou pas).

---

## 18. Ce que l'implémentation a changé au guide

Écarts délibérés entre ce document et `voxloom-shard`. Chacun est motivé, et
motivé *par une règle du guide lui-même* : là où le pseudo-code et une règle se
contredisaient, c'est la règle qui a gagné.

| § | le guide dit | le code fait | pourquoi |
|---|---|---|---|
| 2.2 | `Scope::child(seg) -> Scope` | `-> Option<Scope>` | Au-delà de `MAX_DEPTH`, saturer rendrait à l'enfant la portée de son parent : une fuite de visibilité déguisée en arrondi. Refuser remonte en `BuildError` et garde la vue précédente (§11.7). |
| 3.2 | `channel(parent, name, narrow)` | `channel(parent, key, name, narrow)` | Le guide ne dit jamais d'où vient l'**identité** d'un canal. La déduire du nom est exactement le piège du §15 (une horloge dans le nom brûlerait un ID par seconde). Le flavor la déclare, le nom reste un champ. Un utilisateur n'a pas ce problème : l'`Occupant` *est* son identité. |
| 3.2 | `debug_assert!` sur `channel_link` | `BuildError::LinkAcrossScopes` | Une assertion de debug ne fait rien en release. Fail closed (R6). |
| 5.1 | `UpdateChannel(ChannelPatch)` avec un jeu de liens | `links_added` / `links_removed` | Le client traite `links` non vide comme un remplacement **total** et ignore une liste vide ; `links_add`/`links_remove` sont traités dans leurs propres blocs. Les jeux incrémentaux composent sur un rejeu et n'exigent pas de connaître la vue engagée. REF `mumble/Messages.cpp::msgChannelState`. |
| 9.2 | `version += 1` à chaque tour utile | version incrémentée **seulement** si le delta partagé est non vide | Seul le delta partagé va au journal : un replan lit les vues, un overlay se recalcule. Sans ça, une connexion durablement congestionnée fait tourner la version à chaque tour et pousse le `tail` du journal au-delà de ce dont ses pairs ont besoin. |
| 9.4 | `replan` affecte `conn.see` avant d'envoyer | les trois composantes n'avancent qu'au succès | C'est la règle 6 du §11, que le pseudo-code du §9.4 contredisait. Sinon une connexion congestionnée garderait la nouvelle observation avec l'ancienne vue, et le filtre du tour suivant utiliserait une portée dont le client n'a jamais entendu parler. |
| 3.5 / 8.3 | « vérifier que `r` voit `s` » sur les arêtes | vérification **par portée distincte**, jamais par paire | Matérialiser les paires d'un domaine est quadratique et tournait à *chaque* rendu. À 500 connexions, ces deux passages en `BTreeSet` coûtaient plus que tout le reste du tour (21,6 ms contre 0,74 ms une fois corrigés). `resolve()` reste la définition, et un test y épingle `compile`. |
| 4 | allocateur d'identifiants **par shard** | un seul allocateur pour tout le runtime, canaux indexés sur `(shard, clé)` | Dès qu'une connexion peut changer de shard, l'allocation par shard casse la règle 3 du §11 vue du client : le shard A retire le canal 5, le shard B en crée un autre qui porte aussi le 5. Les sessions sont pires — voir la ligne suivante. Comme la session est indexée sur l'`Occupant`, une migration garde la sienne **gratuitement**. |
| 9.6 | « migrer = détacher de A, attacher à B, **rien d'autre** » | le détachement d'une migration ne pousse **rien** ; A transmet la vue tenue par le client, B planifie une seule transition dessus | Le démontage n'est pas seulement du gaspillage, il **déconnecte le client officiel**. `msgUserRemove` ne retire pas la victime du modèle quand c'est soi (`if (pDst != pSelf)`), donc le `ChannelRemove` qui suit ressemble à la suppression d'un canal occupé ; `msgChannelRemove` journalise « Protocol violation » et appelle `disconnect()`. REF `mumble/Messages.cpp`, `mumble/UserModel.cpp::removeChannel`. |
| 9.2 | table de routage compilée depuis la vue partagée | compilée depuis la vue partagée **plus la présence propre de chaque overlay** | Une connexion sans présence partagée n'est pas absente du runtime : c'est exactement ce qu'est un vanish. L'omettre transformait silencieusement « entend tout, n'est entendu de personne » en « ne participe pas à l'audio », sans que le flavor puisse distinguer les deux. Qui l'entend reste une autre question, et le rendu refuse déjà une relation dont la réponse est non. |
| 10.1 | `route(&identity)` | `route(connection, &identity)` | Un `VoiceEvent` ne transporte qu'un `ConnectionId` — délibérément, le runtime n'a pas d'opinion sur ce qu'est un utilisateur. Le routage est donc le seul instant où l'identité et l'identifiant se rencontrent : une application qui veut que son flavor connaisse un nom enregistre la paire là. |

### 18.1 Ce qui n'est pas fait

- **`voxloom-server` n'est pas rebranché.** Le modèle par connexion (P5–P7) reste
  celui qui tourne. Le rebrancher retirerait le coordinateur de publication et les
  jetons de commit, et casserait les tests de conformité du testkit qui jugent ce
  pipeline — or R2 interdit de toucher `voxloom-testkit/` dans le même diff qu'un
  `voxloom-*/src`. C'est une bascule en deux commits séparés, pas un détail.
- **Utilisateurs synthétiques** : `Occupant::Synthetic` leur donne une session
  stable, mais aucune politique n'est inventée pour la façon dont l'audio les
  référence (§17.5 reste ouvert).
- **Cibles `VoiceTarget` (shout / whisper)** : refusées et journalisées plutôt
  que routées comme de la parole normale, ce qui livrerait de la voix à des
  auditeurs que le client n'a jamais adressés. Leur enregistrement est P9.
- **Resync de nonce OCB2** : un datagramme d'un pair lié qui ne déchiffre plus
  est jeté avec un log. Le `resync` du `Ping` TCP vaut donc 0 en vérité.
- **Le proptest ne consomme pas `SimulatedMumbleClient`** (R2, même raison). Le
  modèle strict de `voxloom-shard/tests/support/model.rs` applique les vrais
  messages de contrôle et juge chaque état intermédiaire ; le brancher sur le
  vérificateur officiel reste à faire.

### 18.2 Mesure

`ci/bench-shard.sh` fait tourner les deux modèles sur le **même** changement
métier (un membre change de realm), aux mêmes tailles. Apple Silicon, profil
release, médiane sur 20 tours :

| connexions | tour de shard | par connexion | publication P7 | par connexion |
|---|---|---|---|---|
| 2 | 3,42 µs | 1,71 µs | 25,96 µs | 12,98 µs |
| 10 | 8,88 µs | 887 ns | 156,67 µs | 15,67 µs |
| 50 | 149,42 µs | 2,99 µs | 1,47 ms | 29,48 µs |
| 200 | 418,75 µs | 2,09 µs | 21,39 ms | 106,97 µs |
| 500 | **736,58 µs** | **1,47 µs** | **205,25 ms** | 410,50 µs |

Ce qu'il faut lire n'est pas le facteur 279 à 500 connexions, c'est la colonne
« par connexion » : elle **descend** dans le modèle à shards (1,71 → 1,47 µs) et
**monte** d'un facteur 32 dans l'ancien. Le delta partagé fait 2 opérations quelle
que soit la taille — c'est toute la thèse, et c'est ce que le binaire imprime.
