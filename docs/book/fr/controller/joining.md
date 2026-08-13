# Connecting a player to Mumble

L'enregistrement d'un participant ne place pas automatiquement le joueur dans le voice chat. La connexion d'un client Mumble reste nécessaire ; cette page en détaille le fonctionnement.

## Mumble join token

Lors de l'attribution de l'ownership d'un participant, le runtime délivre un **Mumble join token** : un mot de passe à usage unique et spécifique à ce participant, permettant à un client Mumble non modifié de se connecter en son nom.

```java
handle.whenMumbleJoinTokenAvailable()
        .thenAccept(token -> sendJoinLink(player, token.value()));
```

Il est également possible de le lire directement lorsque le participant est déjà dans l'état `OWNED` :

```java
Optional<MumbleJoinToken> token = handle.mumbleJoinToken();
```

Le token devient disponible dès l'octroi de l'ownership, généralement quelques millisecondes après l'appel à `registerParticipant`.

## URL de connexion (join URL)

Le moyen le plus simple de connecter un joueur consiste à utiliser un lien `mumble://`. Mumble enregistrant ce schéma d'URL lors de son installation, l'ouverture du lien déclenche le lancement du client et sa connexion automatique :

```java
String url = "mumble://" + player.getName() + ":" + token.value() + "@voice.example.com";
```

Les tokens étant encodés en base64 URL-safe, aucun échappement n'est nécessaire.

Le username présent dans l'URL est **ignoré**. Seul le mot de passe sert à l'identification du participant. L'inclusion du nom du joueur reste recommandée : Mumble l'affiche pendant la connexion et améliore la lisibilité du lien. Le nom visible par les autres participants correspond au `displayName` défini dans la spec.

Dans Minecraft, l'envoi s'effectue sous forme de composant de chat cliquable :

```java
TextComponent link = new TextComponent("Cliquez ici pour rejoindre le voice chat");
link.setClickEvent(new ClickEvent(ClickEvent.Action.OPEN_URL, url));
player.spigot().sendMessage(link);
```

Une implémentation complète est disponible dans [A Minecraft plugin](minecraft.md).

## Gestion sécurisée des jetons

Le join token est un bearer credential. Toute personne disposant de ce jeton a la possibilité de se connecter en tant que ce participant et de s'exprimer en son nom.

- Ne jamais journaliser ni inscrire le token dans un fichier en clair (plaintext).
- Ne jamais le transmettre à un autre utilisateur ; l'envoi doit se faire exclusivement au participant concerné via un canal privé.
- Le token ne confère aucun privilège de contrôle sur la session. La fuite d'un token ne permet ni le déplacement de participants, ni le mute d'autres utilisateurs, ni la lecture de la liste des Spaces. Le dommage potentiel se limite à l'usurpation de ce seul participant.

La méthode `MumbleJoinToken.toString()` masque volontairement sa valeur (redacted), évitant la fuite accidentelle du secret lors de l'exécution de `log.info("token: " + token)`. L'appel à `.value()` est à réserver au moment de l'émission effective du secret.

## Rotation des tokens

Le token est sujet à rotation. Il survit à une simple ré-connexion, mais une nouvelle acquisition du même participant génère un nouveau token et invalide le précédent.

L'émission du token lors du login initial suffit pour la plupart des intégrations. Si le lien reste accessible ou peut être redemandé, l'écoute des événements de rotation s'impose :

```java
handle.addListener(new ParticipantListener() {
    @Override
    public void onMumbleJoinTokenChanged(ParticipantHandle participant, MumbleJoinToken token) {
        updateStoredLink(participant.participantId(), token.value());
    }
});
```

La révocation ou l'unregister d'un participant invalide immédiatement son token.

## Adresses et ports de connexion

Le client ne se connecte pas à l'endpoint gRPC. Le code Java communique avec le port controller (`4000` par défaut, généralement sur localhost), tandis que les joueurs se connectent au port Mumble (`64738` par défaut) sur le hostname public du serveur.

L'URL destinée aux joueurs doit être construite avec ce nom de domaine public, et non avec l'URI transmise à `ControllerSession.builder`.

## Comportement côté joueur

Lors de la connexion, le joueur arrive directement dans le Space désigné par sa spec, sous son nom d'affichage, et entend uniquement les participants déterminés par le runtime. Aucun arbre de canaux global n'est proposé au parcours et aucun changement manuel de Space n'est possible, chaque connexion recevant une vue dédiée.

Le déplacement d'un joueur d'un Space à un autre par l'application est transparent côté client, en dehors de la modification du flux audio perçu. Aucune ré-connexion et aucune interaction de la part de l'utilisateur ne sont nécessaires.
