# Getting started

Cette page explique comment démarrer un controller server, enregistrer un participant depuis Java, et y connecter un vrai client Mumble.

## Prérequis

- **Java 8 ou supérieur.** Le SDK est compilé en bytecode Java 8 et s'exécute sur tout serveur Minecraft à partir de la version 1.8.
- **Une toolchain Rust**, pour build et lancer le controller server.
- **Un client Mumble**, pour vérifier le résultat. Tout client officiel récent convient.

## 1. Ajouter la lib

Le SDK est publié sous forme d'un artefact unique :

```text
be.theking90000.mumble:controller
```

Gradle (Kotlin DSL) :

```kotlin
dependencies {
    implementation("be.theking90000.mumble:controller:0.1.0")
}
```

Gradle (Groovy DSL) :

```groovy
dependencies {
    implementation 'be.theking90000.mumble:controller:0.1.0'
}
```

Maven :

```xml
<dependency>
  <groupId>be.theking90000.mumble</groupId>
  <artifactId>controller</artifactId>
  <version>0.1.0</version>
</dependency>
```

> **Work in progress.** Aucune release publique n'existe pour l'instant ; ces coordonnées ne se résolvent donc sur aucun repository. En attendant la première release, builder l'artefact en local avec `./gradlew publishToMavenLocal` depuis `integrations/controller`, ajouter `mavenLocal()` aux repositories et utiliser la version `0.1.0-SNAPSHOT`.

Le SDK embarque gRPC et Protobuf. Dans un plugin Minecraft, ceux-ci doivent être shadés et relocatés dans le jar final afin d'éviter les conflits de versions entre plugins. La configuration est détaillée dans [A Minecraft plugin](minecraft.md#shading).

## 2. Lancer le controller server

Depuis la racine du repository :

```sh
cargo run -p mumble-controller-server -- --dev-self-signed
```

L'adresse et les ports d'écoute s'affichent :

```text
mumble-controller-server: Controller listening on 127.0.0.1:4000, Mumble listening on 0.0.0.0:64738
```

Deux ports pour deux usages. Le port `4000` est le port gRPC utilisé par le code Java. Le port `64738` est le port Mumble standard sur lequel se connectent les joueurs.

`--dev-self-signed` génère un certificat TLS temporaire à chaque démarrage et sert uniquement pour le dev local. En production, fournir un vrai certificat avec `--mumble-cert` et `--mumble-key`. La liste complète des options se trouve sur la page de référence [Rust server](server.md).

## 3. Première session

```java
import be.theking90000.mumble.controller.*;
import java.net.URI;

public class VoiceDemo {
    public static void main(String[] args) throws Exception {
        ControllerSession session = ControllerSession.builder(
                        ControllerId.of("my-server"),
                        URI.create("http://127.0.0.1:4000"))
                .build();

        session.start().get();

        ParticipantHandle steve = session.registerParticipant(
                ParticipantId.of("steve"),
                ParticipantSpec.builder(SpaceKey.of("lobby"), "Steve").build());

        String token = steve.whenMumbleJoinTokenAvailable().get().value();
        System.out.println("mumble://steve:" + token + "@127.0.0.1");

        Thread.sleep(600_000L);
        session.stop().get();
    }
}
```

Détails sur ce code :

- `ControllerId` nomme l'application. Ne s'agissant pas d'un mot de passe, l'utilisation d'un nom stable (comme le nom du plugin) est recommandée.
- Le schéma d'endpoint est `http` car le port controller est en plaintext en v1. Conserver le controller server sur la même machine que l'application. Le SDK accepte également un endpoint `https` avec `.tls(TlsConfig.systemTrust())` dans le cas d'un déploiement terminant le TLS en amont du serveur.
- `start()` retourne un future qui se complète une fois le runtime prêt et l'état initial réconcilié.
- `registerParticipant` déclare l'existence de Steve dans le Space `lobby` avec le nom d'affichage `Steve`. La méthode retourne immédiatement et l'ownership est accordé de façon asynchrone.
- `whenMumbleJoinTokenAvailable()` fournit le mot de passe nécessaire au client Mumble de Steve. Voir [Connecting a player to Mumble](joining.md).

## 4. Connexion

Exécuter le programme, copier la ligne `mumble://` générée et l'ouvrir. Mumble se lance et se connecte. Le warning de certificat est attendu avec `--dev-self-signed`.

Le channel rejoint porte le nom du Space, et le nom affiché correspond au display name de la spec. En enregistrant un deuxième participant sous un identifiant différent et en ouvrant son URL dans un deuxième client Mumble, les deux participants peuvent échanger.

## Enregistrement avant connexion de la session

Appeler `start()` n'est pas un prérequis pour décrire un état. Les participants et observations enregistrés au préalable sont intégrés dans le snapshot d'ouverture :

```java
ControllerSession session = ControllerSession.builder(id, endpoint).build();
session.registerParticipant(alice, aliceSpec);
session.registerParticipant(bob, bobSpec);
session.start();
```

Un plugin tire parti de ce comportement lorsque `onEnable` doit restaurer un état avant l'établissement de la connexion, ou lorsque des joueurs se connectent pendant l'initialisation de la session.

Bloquer sur `start().get()` est approprié dans une méthode `main`, mais à éviter sur le main thread d'un serveur de jeu. [A Minecraft plugin](minecraft.md) utilise la forme non-bloquante.
