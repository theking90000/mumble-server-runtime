# Troubleshooting

Recensement des échecs courants et de leurs causes.

## La session ne passe jamais en ACTIVE

Journaliser les transitions d'état avant toute tentative de diagnostic :

```java
session.addSessionListener(new ControllerSessionListener() {
    @Override
    public void onStateChanged(ControllerSession session,
                               ControllerSessionState previous,
                               ControllerSessionState current) {
        getLogger().info("voice session: " + previous + " -> " + current);
    }
});
```

| Symptôme observé | Signification |
|---|---|
| Bloqué en `CONNECTING` | Aucun service à l'écoute sur l'endpoint |
| Boucle en `RECONNECTING` | Service joignable, mais le stream s'interrompt |
| `FAILED` | Erreur permanente ; le future retourné par `start()` fournit la cause |
| `CLOSED` | Fermeture explicite ou arrêt ordonné par le serveur |

Points à vérifier dans l'ordre : exécution effective du controller server, correspondance de l'hôte et du port avec les valeurs affichées au démarrage, et utilisation du schéma `http` au lieu de `https` en l'absence de configuration TLS.

## "controller bind ... is not loopback"

```text
controller bind 0.0.0.0:4000 is not loopback; pass
--allow-unauthenticated-controller-network to acknowledge plaintext
unauthenticated access
```

Le port controller est en plaintext et n'authentifie pas les appelants en v1. Tout accès réseau à ce port permet la modification ou le mute des participants. Le serveur refuse en conséquence d'effectuer un bind sur une interface publique sans confirmation explicite.

L'usage du loopback et l'exécution du controller server sur la même machine que le serveur de jeu sont recommandés. Si le franchissement d'un réseau est inévitable, le passage du flag et la protection du port par un pare-feu ou un réseau privé sont nécessaires.

## Échec de connexion d'un joueur à Mumble

**Erreur de certificat ou de mot de passe.** Le serveur remonte l'un des deux motifs suivants :

- *a Mumble join token is required* : le client s'est connecté sans mot de passe. Le mot de passe étant l'unique élément d'identification du participant, sa présence est obligatoire.
- *the Mumble join token is invalid or revoked* : le token est expiré ou obsolète. Une nouvelle acquisition a provoqué sa rotation, ou le participant a fait l'objet d'un unregister ou d'une révocation. Récupérer à nouveau la valeur via `handle.mumbleJoinToken()` et transmettre la valeur courante.

**Avertissement de certificat.** Comportement normal lors de l'utilisation du flag `--dev-self-signed` qui génère un certificat éphémère à chaque démarrage. À valider en environnement de dev, et à remplacer par un certificat valide via `--mumble-cert` et `--mumble-key` dans les autres environnements.

**Erreur Connection refused sur localhost.** Le listener Mumble écoute en IPv4 (`0.0.0.0:64738`). Certains clients résolvant `localhost` en `::1` par défaut ne basculent pas sur IPv4. La connexion explicite vers `127.0.0.1` résout ce problème.

## Joueur connecté mais absence d'audio

Vérifier la valeur de `appliedSpaceKey()` dans le status des deux participants. Toute différence avec la valeur configurée indique l'impossibilité pour le runtime d'appliquer la description, le motif étant fourni par `applicationError()`.

Si les deux participants se trouvent dans le même Space sans pouvoir communiquer, vérifier les paramètres `serverMute` et `serverDeaf` dans les specs, puis `selfMute()` et `selfDeaf()` dans les status. La coupure du micro ou du casque par le joueur lui-même dans son propre client simule de façon identique un dysfonctionnement de l'intégration.

## Exceptions

| Exception | Cause | Solution |
|---|---|---|
| `IllegalStateException` lors de `registerParticipant` | Identifiant déjà enregistré ou libération en cours | Réutiliser `session.participant(id)` ou attendre le future de `unregister()` |
| `SessionClosedException` | Session ou handle en cours d'arrêt ou dans un état terminal | Ne pas réutiliser une session arrêtée ni un handle révoqué (objets à usage unique) |
| `OwnershipLostException` | Ownership pris par une autre application ou expiration du lease | Procéder à un nouvel enregistrement pour obtenir un nouveau handle |
| `CommandRejectedException` | Commande refusée par le runtime ; la règle violée est indiquée par `code()` | Généralement l'atteinte d'une limite (voir ci-dessous) |
| `ControllerException` lors de `fetchSpace` | Session non active à cet instant | Réessayer une fois l'état `ACTIVE` atteint, ou utiliser `observeSpace` |

## Limites (limits)

Le serveur applique des limites de ressources et rejette toute commande entraînant un dépassement. Les valeurs par défaut sont calibrées pour un serveur de jeu unique :

| Limite | Valeur par défaut |
|---|---|
| Participants au total | 10 000 |
| Participants par session | 5 000 |
| Spaces | 1 024 |
| Spaces observés par session | 1 024 |
| Connexions Mumble simultanées | 100 |
| Sessions | 64 |

Le paramètre `--max-mumble-connections` est le premier à ajuster pour un déploiement réel, la limite par défaut de 100 étant réservée au dev. Chaque limite est configurable via un flag en ligne de commande listé sur la page de référence [Rust server](server.md).

## Déconnexion générale lors du redémarrage du plugin

L'ownership est maintenu par un lease (30 secondes par défaut). La perte de la connexion gRPC suspend la session sans éjecter les utilisateurs, permettant à un redémarrage s'achevant dans le délai du lease d'être transparent pour les conversations audio. Tout redémarrage dépassant cette durée entraîne l'expiration du lease, la révocation des participants et la fermeture de leurs connexions Mumble.

L'augmentation de `--lease-seconds` s'impose en cas de cycle de reload plus long.

## Absence de réaction suite à un setSpec

La méthode `setSpec` ne bloque pas et ne lève aucune exception lors d'une coupure de connexion. Elle enregistre la nouvelle description qui sera transmise dès le rétablissement du flux. Ce comportement nominal évite d'avoir à implémenter une logique de retry dans le code applicatif.

Pour vérifier la prise en compte effective d'une description, l'attente du future retourné ou l'écoute de `onStatusChanged` est nécessaire. L'absence d'exception ne garantit pas la livraison immédiate.
