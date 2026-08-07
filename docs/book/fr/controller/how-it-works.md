# Référence : le modèle

> Background. Une intégration fonctionnelle ne nécessite pas la lecture de cette page.

Mumble Controller constitue la frontière indépendante du langage pour les applications détenant des participants sans faire partie du runtime vocal. Un serveur Minecraft est un exemple de controller parmi d'autres ; ni le contrat ni le SDK Java ne dépendent de Minecraft, Bukkit ou Paper.

L'intégration s'appuie sur trois concepts d'identité distincts :

- une **session controller** est un producteur déclaratif en direct ;
- un **participant** est une personne logique détenue par au plus une session ;
- un **Space** est une clé de placement sémantique calculée à partir de l'état du participant.

Les controllers détiennent des participants, jamais des shards du runtime. Plusieur sessions peuvent placer leurs participants au sein d'un même Space. L'application Rust détermine la façon dont ce Space est matérialisé par le runtime, et les clients ne reçoivent jamais de `ShardId` ni de `ConnectionId`.

## État désiré plutôt que commandes

Chaque participant transporte une spécification désirée complète :

```text
ParticipantSpec {
    space_key
    display_name
    server_mute
    server_deaf
}
```

La modification de `space_key` remplace l'intégralité de la spécification. Il n'existe aucune commande move séparée, et aucune commande controller ne crée ni ne mutent directement un shard.

Une session détient également l'ensemble des observations explicites de Spaces. Son périmètre de lecture effectif (read interest) correspond à :

```text
Spaces observés explicitement
UNION Spaces hébergeant au moins l'un de ses participants
```

La commande `FetchSpace` constitue une lecture ponctuelle (one-shot) et ne modifie pas cet ensemble.

## Ownership et credentials de connexion

Rust délivre une capability d'ownership opaque lors de l'acquisition d'un participant par un enregistrement. Toute écriture ou libération ultérieure doit présenter cette même capability. Une nouvelle acquisition la remplace, empêchant une libération retardée émanant de l'ancien owner de détacher le nouveau.

Le token de connexion Mumble (join token) fourni lors d'un octroi d'ownership constitue un bearer credential distinct. Il permet à un client Mumble non modifié de s'authentifier au nom de ce participant, sans accorder de droit d'écriture sur le controller. Les applications doivent le traiter comme un mot de passe et ne jamais le journaliser.

La surveillance de la connexion de transport et le lease métier fonctionnent de manière indépendante. La perte d'un stream gRPC n'entraîne pas la suppression immédiate de l'ownership : le SDK Java se reconnecte en transmettant son snapshot complet tant que le lease serveur demeure valide.

## Trois jalons (watermarks)

La réussite d'une écriture gRPC ne garantit pas la réception de la nouvelle vue par un client Mumble. Le protocole maintient en conséquence trois jalons distincts :

```text
accepted  -> l'application Controller a accepté la révision désirée
applied   -> un rendu runtime valide a consommé cette révision
published -> la génération Mumble produite par ce rendu
```

## Structure du repository

Le contrat et le SDK Java sont situés dans le répertoire `integrations/controller` :

- `contract` contient la définition canonique versionnée Protobuf/gRPC. Le digest de son descripteur compilé étant contrôlé par le CI, toute modification incompatible avec le protocole réseau provoque l'échec du build.
- `sdk-java` contient le SDK compatible Java 8.
- `server-rust` contient le serveur composé décrit sur la page [Référence : le serveur Rust](server.md).

Voir la page [Référence : cycles de vie](java.md) pour le détail des machines à états des sessions et des participants.
