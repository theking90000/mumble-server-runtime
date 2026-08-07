# Participants

Un participant représente une personne physique ou logique gérée par l'application. Cette page détaille l'enregistrement, la modification, la surveillance et la suppression d'un participant.

## Identifiant et display name

Un participant possède deux noms aux usages distincts :

```java
ParticipantId.of("069a79f4-44e9-4726-a5be-fca90e38aaf5")   // identifiant interne
ParticipantSpec.builder(SpaceKey.of("lobby"), "Notch")      // nom affiché aux joueurs
```

`ParticipantId` constitue la clé utilisée par le code de l'application. Cet identifiant devant rester strictement immuable pour une même personne, l'usage d'un UUID Minecraft, d'une clé primaire de base de données ou d'un identifiant de compte est recommandé. Il n'est jamais affiché aux utilisateurs.

Le display name correspond au nom d'affichage visible par les autres joueurs dans leur client Mumble. Il peut être modifié librement et n'a pas l'obligation d'être unique.

## Enregistrement

```java
ParticipantHandle handle = session.registerParticipant(
        ParticipantId.of(player.getUniqueId().toString()),
        ParticipantSpec.builder(SpaceKey.of("lobby"), player.getName()).build());
```

`registerParticipant` retourne immédiatement un `ParticipantHandle`, avant la confirmation du runtime. Toutes les opérations ultérieures sur ce participant s'effectuent via ce handle, d'où la nécessité de le conserver (par exemple dans une map indexée par l'identifiant du joueur).

L'enregistrement ne bloque pas et n'échoue pas en cas de connexion interrompue. Si la session n'est pas encore connectée, l'enregistrement est intégré au snapshot transmis lors de l'ouverture de la connexion.

Tout second appel avec un identifiant identique alors que le premier enregistrement est toujours actif lève une `IllegalStateException`. Il convient soit de procéder à un unregister au préalable, soit de récupérer le handle existant via `session.participant(id)`.

## Spécification (spec)

`ParticipantSpec` regroupe la description complète d'un participant en quatre champs :

| Champ | Signification |
|---|---|
| `spaceKey` | Space auquel appartient ce participant |
| `displayName` | Nom affiché dans le client Mumble des autres joueurs |
| `serverMute` | Interdiction d'émettre du son |
| `serverDeaf` | Interdiction de recevoir du son |

```java
ParticipantSpec spec = ParticipantSpec.builder(SpaceKey.of("team-red"), "Notch")
        .serverMute(true)
        .build();
```

`serverMute` et `serverDeaf` correspondent à des restrictions imposées par le serveur. Elles se distinguent des états mute et deafen activés par le joueur dans son propre client Mumble, lesquels sont lisibles mais non modifiables. Voir [Réagir aux actions des joueurs](#reacting-to-players).

## Modification d'un participant

Un call unique remplace l'intégralité de la description :

```java
handle.setSpec(ParticipantSpec.builder(SpaceKey.of("team-blue"), "Notch").build());
```

Modifier la valeur de `spaceKey` déplace le participant. Il n'existe pas d'opération distincte de move, rename ou mute : il suffit de construire la description cible et de l'appliquer.

La description étant remplacée dans son ensemble, sa construction s'appuie sur l'état courant de l'application plutôt que sur une spec antérieure. Le calcul centralisé de la spec est généralement à privilégier :

```java
private ParticipantSpec specFor(Player player) {
    return ParticipantSpec.builder(spaceOf(player), player.getName())
            .serverMute(isSilenced(player))
            .build();
}

// lors de tout changement d'état du jeu
handle.setSpec(specFor(player));
```

L'appel peut être répété aussi souvent que nécessaire. Définir deux fois la même valeur est sans effet secondaire, et les modifications rapides et successives sont coalescées afin de ne transmettre que la description la plus récente.

### Suivi de la prise d'effet des modifications

`setSpec` retourne un future qu'il est souvent possible d'ignorer. Lorsque son suivi est requis, le future se complète avec une `AcceptedRevision` contenant trois jalons distincts :

```java
handle.setSpec(spec).thenAccept(revision -> {
    revision.acceptedSpecRevision();   // le runtime a accepté la description
    revision.appliedSpecRevision();    // la description a servi à calculer un nouveau layout
    revision.publishedGeneration();    // le layout a été transmis aux clients Mumble
});
```

Ces jalons sont séparés car la réussite d'un appel ne garantit pas la mise à jour immédiate des clients. La majorité des logiques applicatives n'a pas besoin de suivre ces jalons. Leur usage est réservé aux diagnostics ou aux cas où une action dépend de la visibilité effective d'un déplacement.

## Suppression d'un participant

```java
handle.unregister();
```

Le participant quitte son Space et sa connexion Mumble est fermée. Cette action est à exécuter lorsqu'un joueur quitte le serveur.

L'unregister est définitif pour le handle concerné. En cas de retour du joueur, un nouvel appel à `registerParticipant` est nécessaire pour obtenir un nouveau handle.

## Réagir aux actions des joueurs

Le runtime remonte les actions effectives des participants via l'interface `ParticipantListener`. Chaque méthode disposant d'une implémentation par défaut, il suffit de surcharger celles qui sont nécessaires :

```java
handle.addListener(new ParticipantListener() {
    @Override
    public void onStatusChanged(ParticipantHandle participant, ParticipantStatus status) {
        if (status.mumbleConnected()) {
            // le client Mumble est connecté
        }
        if (status.selfMute()) {
            // le joueur s'est mute lui-même dans son client
        }
    }

    @Override
    public void onOwnershipLost(ParticipantHandle participant, String reason) {
        // une autre application a pris ce participant, ou le lease a expiré
    }
});
```

`ParticipantStatus` remonte la réalité observée, tandis que la spec traduit l'intention applicative :

| Méthode | Information remontée |
|---|---|
| `mumbleConnected()` | Indique si un client Mumble est actuellement connecté |
| `appliedSpaceKey()` | Space dans lequel se trouve réellement le participant |
| `selfMute()`, `selfDeaf()` | État configuré par le joueur dans son propre client |
| `applicationError()` | Motif de l'échec de prise en compte d'une description, le cas échéant |

La lecture de `selfMute()` permet par exemple d'afficher une icône de sourdine au-dessus d'un joueur, ou de restreindre une fonctionnalité vocale en cas de coupure du micro par le joueur.

> **Les callbacks ne s'exécutent pas sur le main thread.** Ils sont distribués par l'executor de callbacks de la session, sérialisés dans l'ordre. Aucun blocage ne doit y être effectué, et un retour sur le thread principal du jeu est requis avant de manipuler l'état du jeu. Dans Bukkit, ce retour s'effectue via `Bukkit.getScheduler().runTask(plugin, ...)`.

## États d'un Handle

`handle.state()` indique la position d'un participant dans son cycle de vie d'ownership.

| État | Signification | Conduite à tenir |
|---|---|---|
| `ACQUIRING` | Enregistré, en attente de prise en compte par le runtime | Aucune action, état nominal |
| `OWNED` | Acquis et opérationnel | Aucune action |
| `SUSPENDED` | Connexion perdue, ownership maintenu | Aucune action, rétablissement automatique |
| `REVOKED` | Perte permanente de l'ownership | Ré-enregistrer si le joueur est toujours en ligne |
| `CLOSED` | Unregister exécuté | Aucune action |

`SUSPENDED` n'implique pas la déconnexion du joueur du canal vocal. L'ownership étant maintenu par un lease serveur qui survit à la connexion gRPC, une brève coupure réseau ou un redémarrage du controller n'interrompt pas la conversation. L'état souhaité est conservé et réémis lors de la ré-connexion.

L'état `REVOKED` est définitif. Il survient lorsqu'une autre application enregistre le même `ParticipantId`, ou lors de l'expiration du lease suite à une déconnexion prolongée. Le handle devenant inutilisable, un nouvel enregistrement est nécessaire.
