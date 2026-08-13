# A Minecraft plugin

Les pages précédentes traitent du Java standard et s'appliquent à toute application. Cette page les rassemble au sein d'un plugin Bukkit fonctionnel, représentant le cas d'usage le plus courant.

Rien dans cette implémentation n'est imposé par le SDK, qui ne possède aucune dépendance Minecraft. Un bot Discord ou un backend web s'appuie sur la même API.

## Fonctionnalités du plugin

- Ouverture d'une session controller au démarrage du serveur.
- Enregistrement d'un participant à la connexion d'un joueur et transmission d'un lien de join cliquable.
- Déplacement des participants entre les Spaces lors des changements de monde.
- Unregister du participant au départ du joueur.
- Fermeture propre de la session lors de l'arrêt du serveur.

## Implémentation

```java
package com.example.voice;

import be.theking90000.mumble.controller.*;
import net.md_5.bungee.api.chat.ClickEvent;
import net.md_5.bungee.api.chat.TextComponent;
import org.bukkit.Bukkit;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.Listener;
import org.bukkit.event.player.PlayerChangedWorldEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.plugin.java.JavaPlugin;

import java.net.URI;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

public final class VoicePlugin extends JavaPlugin implements Listener {

    private final Map<UUID, ParticipantHandle> handles = new ConcurrentHashMap<UUID, ParticipantHandle>();
    private ControllerSession session;
    private String publicHost;

    @Override
    public void onEnable() {
        saveDefaultConfig();
        publicHost = getConfig().getString("public-host", "127.0.0.1");
        String endpoint = getConfig().getString("controller-endpoint", "http://127.0.0.1:4000");

        session = ControllerSession.builder(
                        ControllerId.of("example-voice"),
                        URI.create(endpoint))
                .build();

        // Ne pas bloquer le thread du serveur. Les joueurs qui se connectent avant
        // l'établissement de la session sont enregistrés correctement : leur
        // enregistrement fait partie du snapshot d'ouverture.
        session.start().whenComplete((ignored, failure) -> {
            if (failure != null) {
                getLogger().severe("Voice chat unavailable: " + failure.getMessage());
            } else {
                getLogger().info("Voice chat connected.");
            }
        });

        getServer().getPluginManager().registerEvents(this, this);
    }

    @Override
    public void onDisable() {
        if (session == null) {
            return;
        }
        try {
            session.stop().get(5, TimeUnit.SECONDS);
        } catch (Exception failure) {
            getLogger().warning("Voice chat did not shut down cleanly: " + failure);
        }
    }

    @EventHandler
    public void onJoin(PlayerJoinEvent event) {
        Player player = event.getPlayer();
        ParticipantHandle handle = session.registerParticipant(
                ParticipantId.of(player.getUniqueId().toString()),
                specFor(player));
        handles.put(player.getUniqueId(), handle);

        handle.whenMumbleJoinTokenAvailable().thenAccept(token -> {
            String url = "mumble://" + player.getName() + ":" + token.value() + "@" + publicHost;
            // Retour sur le thread du serveur avant toute manipulation de l'instance Player.
            Bukkit.getScheduler().runTask(this, () -> {
                if (player.isOnline()) {
                    TextComponent link = new TextComponent("Cliquez ici pour rejoindre le voice chat");
                    link.setClickEvent(new ClickEvent(ClickEvent.Action.OPEN_URL, url));
                    player.spigot().sendMessage(link);
                }
            });
        });
    }

    @EventHandler
    public void onChangedWorld(PlayerChangedWorldEvent event) {
        ParticipantHandle handle = handles.get(event.getPlayer().getUniqueId());
        if (handle != null) {
            handle.setSpec(specFor(event.getPlayer()));
        }
    }

    @EventHandler
    public void onQuit(PlayerQuitEvent event) {
        ParticipantHandle handle = handles.remove(event.getPlayer().getUniqueId());
        if (handle != null) {
            handle.unregister();
        }
    }

    private ParticipantSpec specFor(Player player) {
        return ParticipantSpec.builder(
                        SpaceKey.of("world-" + player.getWorld().getName()),
                        player.getName())
                .build();
    }
}
```

Fichier de configuration `config.yml` associé :

```yaml
controller-endpoint: "http://127.0.0.1:4000"
public-host: "voice.example.com"
```

Le code ci-dessus constitue une intégration de base complète. Les logiques spécifiques additionnelles prennent place dans la méthode `specFor`.

## Gestion des threads

Deux règles essentielles.

**Ne jamais bloquer le thread du serveur.** Les méthodes `start()`, `setSpec()`, `unregister()` et `observeSpace()` retournent toutes des futures non-bloquants. Le seul appel synchrone à attendre est `stop()` dans `onDisable`, assorti d'un timeout.

**Les callbacks ne s'exécutent pas sur le thread du serveur.** Les callbacks des listeners et continuations de futures s'exécutent sur l'executor de callbacks du SDK. La lecture d'un `ParticipantStatus` y est autorisée, mais toute manipulation d'un `Player`, de l'environnement ou de l'API Bukkit est proscrite : un retour sur le thread du serveur via `Bukkit.getScheduler().runTask(plugin, ...)` est obligatoire, comme illustré dans `onJoin`.

Un executor spécifique peut être fourni au builder si l'exécution des callbacks nécessite un contexte particulier :

```java
ControllerSession.builder(id, endpoint)
        .callbackExecutor(myExecutor)
        .build();
```

Il convient d'éviter de transmettre un executor exécutant les tâches sur le thread principal du serveur. Les callbacks étant sérialisés, un callback bloquant interrompt le traitement de l'ensemble de la session.

## Shading

Le SDK inclut gRPC et Protobuf. Si un autre plugin du même serveur embarque des versions différentes, la première chargée prévaut et risque d'entraîner une rupture d'exécution. Les deux bibliothèques doivent être shadées et relocatées dans le jar final.

Exemple avec le plugin Gradle Shadow :

```kotlin
plugins {
    id("com.gradleup.shadow") version "8.3.5"
}

dependencies {
    implementation("be.theking90000.mumble:controller:0.1.0")
}

tasks.shadowJar {
    relocate("io.grpc", "com.example.voice.libs.grpc")
    relocate("com.google.protobuf", "com.example.voice.libs.protobuf")
    relocate("com.google.common", "com.example.voice.libs.guava")
    relocate("io.perfmark", "com.example.voice.libs.perfmark")
}
```

L'option `minimize()` ne doit pas être activée. gRPC résolvant les transports et codecs via des service loaders et de la réflexion, la minimisation supprime des classes sans référence statique explicite.

## Perspectives d'extension

**Proximity chat.** Calculer la clé de Space à partir d'un découpage du monde en grille large (coarse grid) et appeler `setSpec` lors du franchissement des limites de cellules plutôt qu'à chaque tick.

**Team chat.** Clé basée sur le nom d'équipe. Le changement d'équipe s'effectue par un unique appel à `setSpec`.

**Admin mute.** Passer `serverMute(true)` dans la spec. Pour afficher un indicateur pour les joueurs ayant coupé leur micro eux-mêmes, lire `selfMute()` dans `ParticipantStatus` lors de `onStatusChanged`.

**Commande `/voice`.** Renvoyer le lien de connexion en lisant `handle.mumbleJoinToken()`, ou lister le contenu d'un Space via `session.fetchSpace(...)`.

**Voice multi-serveurs.** Deux serveurs Minecraft pointant vers un même controller server et partageant une même convention de clés placent leurs joueurs dans un Space commun. Chaque serveur conserve la propriété (ownership) de ses propres joueurs.
