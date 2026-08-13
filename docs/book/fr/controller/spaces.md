# Spaces

Un Space est un canal Mumble.

## Choix des clés

Une `SpaceKey` est une string définie au choix :

```java
SpaceKey.of("lobby")
SpaceKey.of("team-red")
SpaceKey.of("arena-3")
SpaceKey.of("world:overworld/region:12,-4")
```

Le choix d'une clé calculable à partir de l'état interne permet de faire du placement de joueur une simple fonction. Le reste du code n'a ainsi jamais besoin de mémoriser l'emplacement des joueurs :

```java
private SpaceKey spaceOf(Player player) {
    Team team = teamOf(player);
    return team == null
            ? SpaceKey.of("lobby")
            : SpaceKey.of("team-" + team.getName());
}
```

## Lifetime

Un Space commence à exister lorsque le premier participant y est placé, et cesse d'exister peu après le départ du dernier. Le delay par défaut est de 30 secondes, évitant ainsi la destruction et la reconstruction d'un Space en cas de simple passage furtif. Il n'existe aucun call create ni call delete.

Réutiliser une clé après la fermeture de son Space produit un nouveau Space et non la réouverture de l'ancien. Il porte une `SpaceIncarnation` différente, empêchant la lecture d'un snapshot obsolète comme s'il s'agissait de l'état actuel.

## Obtenir la liste des membres d'un Space

Utile pour une commande `/voice list`, un scoreboard ou un affichage d'administration.

Un Space hébergeant au moins un participant de l'application est toujours visible. Pour lire un Space n'en contenant aucun, l'observation doit être explicitement demandée :

```java
session.observeSpace(SpaceKey.of("lobby"));
```

Le cache peut ensuite être consulté à tout moment :

```java
SpaceSnapshot lobby = session.spaces().get(SpaceKey.of("lobby"));
if (lobby != null) {
    for (SpaceParticipant participant : lobby.participants()) {
        System.out.println(participant.displayName()
                + (participant.mumbleConnected() ? " (connected)" : " (away)"));
    }
}
```

Les modifications peuvent également être poussées sous forme de notifications :

```java
session.addSpaceListener(new SpaceListener() {
    @Override
    public void onSpaceUpdated(ControllerSession session, SpaceSnapshot snapshot) {
        refreshScoreboard(snapshot);
    }
});
```

Chaque `SpaceSnapshot` constitue un remplacement complet et non un patch. La valeur précédente est jetée au profit de la nouvelle.

`observeSpace` et `unobserveSpace` n'affectent que les Spaces explicitement demandés. Retirer l'observation d'un Space hébergeant encore l'un des participants de l'application n'interrompt pas ses mises à jour, ce Space restant visible en toutes circonstances.

## Lectures ponctuelles (one-off)

`fetchSpace` lit un Space une seule fois, sans abonnement :

```java
session.fetchSpace(SpaceKey.of("arena-3"))
        .thenAccept(snapshot -> showTo(admin, snapshot));
```

À utiliser pour les commandes répondant à une requête ponctuelle. Cette méthode n'affecte pas le cache, n'impacte pas les updates futures, ne fait l'objet d'aucun retry automatique, et échoue en cas de coupure de connexion avant la réception de la réponse. Elle nécessite une session active, contrairement à `observeSpace` qui peut être déclaré à tout moment.

L'utilisation d' `observeSpace` est recommandée pour les affichages continus, et celle de `fetchSpace` pour les affichages uniques.

## Contenu d'un snapshot

`SpaceSnapshot` décrit un Space à un instant T :

| Méthode | Signification |
|---|---|
| `spaceKey()` | La clé demandée |
| `participants()` | L'ensemble des membres, y compris ceux détenus par d'autres applications |
| `incarnation()` | L'incarnation concrète de cette clé |
| `spaceRevision()` | Le compteur de version au sein de cette incarnation |
| `publishedGeneration()` | La génération Mumble reflétée |

Chaque `SpaceParticipant` fournit `participantId()`, `displayName()`, `serverMute()`, `serverDeaf()` et `mumbleConnected()`.

Un participant enregistré dont le joueur n'a pas encore ouvert le client Mumble apparaît dans le snapshot avec `mumbleConnected()` à false. Il est déclaré mais non présent dans le voice.

Pour ordonner deux snapshots d'une même clé, la comparaison porte d'abord sur `incarnation()`, puis sur `spaceRevision()` uniquement si les incarnations sont identiques. Les révisions d'incarnations différentes correspondent à des séquences distinctes.

## Partage d'un Space entre applications

Plusieurs sessions controller peuvent placer des participants dans un même Space, permettant à ces derniers de s'entendre. Chaque session n'est owner que des participants qu'elle a enregistrés : elle voit les autres dans le snapshot sans pouvoir les move, mute ou delete.

Deux plugins gérant des joueurs distincts et partageant une convention de nommage des clés fonctionnent ainsi conjointement sans nécessiter de coordination directe.
