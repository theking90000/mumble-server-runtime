# Intégration du Controller

L'intégration du Controller place les utilisateurs d'une application en voice chat, sans avoir à écrire de code audio.

L'application conserve la partie métier déjà existante : l'identité des utilisateurs, leur nom d'affichage et les règles d'écoute. Le runtime prend en charge le protocole Mumble, le TLS, la voice UDP, le chiffrement, les channel views par utilisateur et le routing audio.

Les joueurs se connectent avec un client Mumble non modifié. Aucun mod ni resource pack n'est nécessaire.

## À qui s'adresse cette doc

Le cas d'usage principal concerne un serveur Minecraft ajoutant du voice chat de proximité ou d'équipe ; les exemples sont rédigés dans ce contexte. Rien dans la lib n'est toutefois lié à Minecraft. Le SDK n'a aucune dépendance Bukkit, Paper, Spigot ou Minecraft, et fonctionne à l'identique dans un bot Discord ou un backend web.

Une connaissance de Java suffit. Aucune connaissance préalable en audio, codecs, protocoles réseau ou spécification Mumble n'est requise.

## Les deux process

Une intégration comporte deux moitiés qui communiquent via gRPC :

```text
your application                the runtime
+---------------------+         +--------------------------+
| your plugin         |  gRPC   | mumble-controller-server |   TLS/UDP   Mumble
| + controller SDK    | <-----> | (Rust)                   | <---------> clients
+---------------------+         +--------------------------+
```

`mumble-controller-server` est un process Rust s'exécutant à côté du game server. Il communique en Mumble avec les joueurs et en gRPC avec l'application.

Le SDK Java (`be.theking90000.mumble:controller`) est la lib à ajouter au plugin. `ControllerSession` représente la quasi-totalité de l'API.

## Spaces, participants, sessions

Trois concepts couvrent toute l'API.

Un **Space** est un canal Mumble. Son nom est une string au choix (`lobby`, `team-red`, `arena-3`). Un Space existe tant qu'au moins une personne s'y trouve. Il n'est ni créé ni supprimé explicitement.

Un **participant** est une personne gérée par l'application : un joueur, un utilisateur, un bot. Un identifiant immuable lui est attribué, puis il est placé dans un Space avec un display name. Il existe aussi longtemps que spécifié par l'application, que le client Mumble soit actuellement connecté ou non.

Une **session** est la connexion de l'application au runtime, et le owner des participants enregistrés. Une instance de `ControllerSession` par instance de plugin constitue le setup classique.

## Description plutôt que commande

Aucune instruction directe du type « déplacer Steve dans le channel team-red » n'est envoyée. Seul l'état devant s'appliquer est décrit :

```java
handle.setSpec(ParticipantSpec.builder(SpaceKey.of("team-red"), "Steve").build());
```

Steve est placé dans `team-red` et s'affiche sous le nom `Steve`. C'est sa description complète. Le runtime la compare à l'état actuel et effectue les ajustements nécessaires pour réduire l'écart.

Trois conséquences pour le code :

- Aucun call move, rename, mute ou kick n'existe. Un seul call remplace la description d'un participant, et modifier le `spaceKey` le déplace.
- Aucun retry n'est nécessaire. Si la connexion tombe pendant une update, le SDK se re-connecte et envoie la description actuelle, sans historique d'opérations pending. Les états dépassés sont ignorés.
- Aucun suivi de synchronisation n'est requis. La description est appliquée dès que l'état interne change, aussi souvent que la logique de jeu le demande.

## Prochaines étapes

La section [Getting started](getting-started.md) contient un programme fonctionnel d'environ 30 lignes. Ensuite :

- [Spaces](spaces.md) couvre le nommage des Spaces, leur lifetime et la lecture de leurs occupants.
- [Participants](participants.md) couvre l'enregistrement, les moves, les mutes, les deletes et les réactions aux événements des joueurs.
- [Connecting a player to Mumble](joining.md) explique comment un joueur rejoint le voice.
- [A Minecraft plugin](minecraft.md) présente un plugin Bukkit complet mettant en œuvre l'ensemble des concepts.
- [Troubleshooting](troubleshooting.md) liste les erreurs classiques.

Les pages préfixées par **Reference** décrivent le modèle de protocole, les state machines de lifecycle et les internals du serveur Rust. Il s'agit de documentation de référence, non requise pour réaliser une intégration fonctionnelle.

## Statut

Cette intégration est en pre-1.0 et évolue encore. L'artefact Java n'est pas encore publié sur un repository public, et les détails peuvent varier d'une version à l'autre. Le contrat de protocole est versionné (`mumble.controller.v1`) et son digest de descripteur compilé est pinné dans le CI, garantissant que la wire compatibility ne peut pas casser silencieusement.
