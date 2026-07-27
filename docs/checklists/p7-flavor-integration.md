# Phase 7 : checklist du point de contrôle humain (flavor de référence en live)

> Roadmap P7 T8, critère de done : le scénario Aurora/Borealis rejoué
> intégralement à travers l'API publique de flavor, sans aucune branche métier
> dans `voxloom-server`, avec les mêmes propriétés observées en P6. Les critères
> machine sont verts. Cette checklist reste à dérouler et signer par un humain
> sur deux vrais clients Mumble.

Ce que la CI prouve déjà, donc hors de cette checklist :

- Une génération est rendue pour toutes les connexions à partir d'un seul
  `Arc<Snapshot>` figé, validée en passes séparées (vues, routes, interactions),
  et rejetée entièrement à la première erreur (`voxloom-control`).
- La publication respecte l'ordre de sécurité : une route révoquée disparaît de
  la table de routage avant que la vue qui la justifiait ne change ; une route
  nouvelle n'est activée qu'après le commit de la vue du destinataire
  (`voxloom-control`, `publication_order`).
- Une connexion dont la file refuse la transition garde sa vue engagée sans
  bloquer les autres, et repart à la génération suivante.
- Les actions client résolues deviennent des `VoiceEvent` versionnés ; une
  interaction dont ce runtime ne sait pas exprimer la charge est refusée, jamais
  inventée (`voxloom-control`, `voice_events`).
- Le flavor de référence possède ses realms et ses mutations, et ne parle au
  runtime que par le contrat public (`voxloom-flavor-reference`).
- Aucune sortie ne référence une entité absente de la vue du destinataire, sur
  tous les canaux de la section 26.7, y compris sous charge audio pendant une
  révocation (`voxloom-testkit`, `tests/flavor_privacy.rs`).
- Aucune crate centrale ne contient de concept du flavor de référence, ni ne
  dépend de son crate (`ci/gates.sh`, `ci/dep-direction.sh`).
- Le coût d'une publication complète est mesuré pour 2 à 500 connexions
  (`ci/bench-publication.sh`).

Ce que seul un humain peut valider : le comportement réel du client officiel
pendant les transitions produites par le flavor, et le fait que rien n'a régressé
par rapport à P6 alors que tout le chemin a changé.

---

## Pré-requis

- Deux clients officiels Mumble 1.5+, idéalement sur deux comptes système.
- Une identité client persistante et distincte dans chaque client.
- Un casque ou deux sorties audio séparées pour éviter le larsen.
- Le binaire de composition : `cargo build -p voxloom-aurora --release`.

`voxloom-server` n'a plus de binaire : le runtime ne choisit pas de flavor. Le
binaire à lancer est celui qui compile le flavor de référence avec lui.

## Lancement

```bash
./target/release/voxloom-aurora --tcp 0.0.0.0:64738
```

Se connecter à `127.0.0.1:64738`, pas `localhost`. Accepter le certificat
serveur auto-signé. Utiliser exactement ces noms, dont le suffixe choisit le
realm initial tout en restant absent du nom affiché :

- client A : `alice@aurora`
- client B : `bob@borealis`

---

## 1. Le scénario P6 est reproduit à l'identique

- [x] Alice voit `Your realm · Aurora` et `Switch to · Borealis`.
- [x] Bob voit `Your realm · Borealis` et `Switch to · Aurora`.
- [x] Alice ne voit pas Bob et Bob ne voit pas Alice.
- [x] Le nom affiché est `alice` et `bob` : le suffixe de realm n'apparaît nulle
      part dans l'interface.
- [x] Le nom du canal racine est celui passé en `--name`.

## 2. Le changement de snapshot est live

- [x] Alice entre dans `Switch to · Borealis`. Elle voit Bob apparaître, et son
      arbre se réétiquette (`Your realm · Borealis`).
- [x] Bob voit Alice apparaître sans reconnexion et sans clignotement de son
      propre canal.
- [x] Les deux s'entendent dans les deux sens.
- [x] Alice repart dans Aurora : chacun voit l'autre disparaître, et le silence
      revient immédiatement, avant la disparition visuelle si l'on écoute
      attentivement une phrase en cours.
- [x] Aucun message d'erreur, aucune déconnexion, aucun gel de l'interface
      pendant ces transitions.

## 3. Rien ne fuit entre realms

- [x] Depuis le menu contextuel, Alice ne peut cibler aucun utilisateur d'un
      autre realm : ils ne sont pas dans sa liste.
- [x] Un aller-retour rapide (5 changements de realm de suite) laisse les deux
      vues cohérentes et les identités stables.
- [x] Après un aller-retour, les préférences locales de Bob sur Alice (volume,
      surnom local) sont toujours attachées à la même personne.

## 4. Le repli et la reconnexion tiennent toujours

- [x] Bloquer l'UDP d'un client (pare-feu local) : l'audio bascule en tunnel TCP
      et les deux continuent de s'entendre.
- [x] Déconnecter Bob : Alice le voit disparaître et ne reçoit plus rien de lui.
- [x] Reconnecter Bob avec le même certificat : il retrouve son realm de départ
      d'après son nom, et les deux vues redeviennent cohérentes.

## 5. Coût observé

- [x] `ci/bench-publication.sh` a été lancé sur cette machine, et le tableau est
      recopié ci-dessous.

```text
== Publication complète (rendu + validation + plan + commit) ==
  conns        median  per connection         worst
      2       25.75µs         12.88µs       45.04µs
     10      140.58µs         14.06µs      172.33µs
     50        1.35ms         26.90µs        2.59ms
    200       21.01ms        105.04µs       24.22ms
    500      198.21ms        396.42µs      216.63ms
```

---

## Signature

OK!
