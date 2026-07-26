# Phase 6 : checklist du point de contrôle humain (vues dynamiques)

> Roadmap P6, critère de done : Alice et Bob voient des arbres différents qui
> divergent à chaud sans reconnexion ; le client officiel supporte les IDs
> locaux, les renommages et les déplacements synthétiques ; les préférences
> locales restent attachées à l'identité stable. Les critères machine sont
> verts. Cette checklist reste à dérouler et signer par un humain sur deux vrais
> clients Mumble.

Ce que la CI prouve déjà, donc hors de cette checklist :

- Toute commande ciblée résout ses Channel IDs et sessions dans la vue engagée
  de son émetteur. Un ID seulement alloué mais jamais livré reste invisible ;
  les blobs et interfaces d'administration ne consultent aucun registre global
  (`voxloom-session`, `inbound`).
- Deux clients simulés dans des realms différents reçoivent des arbres et des
  ensembles d'utilisateurs différents. Un déplacement à chaud fait converger
  les deux modèles, puis les fait diverger de nouveau, sans violation de la
  section 20 (`voxloom-testkit`, `tests/p6_live_views.rs`).
- Le même scénario vérifie l'isolation audio inter-realm, puis l'activation
  après introduction visuelle et la coupure avant retrait visuel.
- Une file réelle presque pleine refuse une transition entière, ne livre aucun
  préfixe, puis converge vers le dernier désiré une fois drainée.
- Le juge vérifie toutes les références d'entités sur les sorties contrôle,
  l'audio UDP et l'audio tunnelisé.

Ce que seul un humain peut valider : le comportement de l'interface du client
officiel face aux renommages, suppressions et réintroductions, ainsi que la
persistance effective des préférences stockées dans sa base locale.

---

## Pré-requis

- Deux clients officiels Mumble 1.5+, idéalement sur deux comptes système.
- Une identité client persistante et distincte dans chaque client. Vérifier dans
  les réglages de certificat que l'identité n'est pas supprimée à la connexion.
  La reconnexion de Bob doit réutiliser exactement le même certificat client.
- Un casque ou deux sorties audio séparées pour éviter le larsen.
- Le binaire : `cargo build -p voxloom-server --release`.

Le serveur demande le certificat client de façon optionnelle. Quand il est
présent, `UserState.hash` contient le SHA-1 en minuscules du DER du certificat
immédiat, comme Murmur. Cette valeur sert uniquement à l'identité de
présentation et aux préférences locales ; elle n'accorde aucun droit. Un client
sans certificat peut se connecter, mais les cases de persistance ne peuvent
alors pas être validées.

## Lancement

```bash
./target/release/voxloom-server --tcp 0.0.0.0:64738
```

Se connecter à `127.0.0.1:64738`, pas `localhost`. Accepter le certificat
serveur auto-signé. Utiliser exactement ces noms, dont le suffixe choisit le
realm initial tout en restant absent du nom affiché :

- client A : `alice@aurora`
- client B : `bob@borealis`

Le scénario expose toujours deux canaux synthétiques. Pour chaque viewer, son
canal s'appelle `Your realm · ...` et l'autre `Switch to · ...`. Les IDs sont
alloués dans un ordre fixe à chaque connexion.

---

## Checklist

- [x] **Vues initiales divergentes.** Alice voit `Your realm · Aurora` et
      `Switch to · Borealis` ; Bob voit les libellés inverses. Chacun ne voit que
      lui-même dans la liste d'utilisateurs.
- [x] **Isolation initiale.** Alice et Bob parlent en mode normal : aucun
      n'entend l'autre. Le loopback serveur reste fonctionnel séparément.
- [x] **Déplacement à chaud.** Bob double-clique le canal Aurora. Il n'y a
      aucune reconnexion TLS ni nouveau dialogue de certificat. Ses deux canaux
      sont renommés à chaud : `Your realm` et `Switch to` s'inversent.
- [x] **Convergence visuelle.** Alice voit Bob apparaître dans Aurora et Bob voit
      Alice. Aucun utilisateur n'apparaît dans un canal inexistant, aucune
      erreur client et aucun arbre cassé ne sont observés.
- [x] **Activation audio après la vue.** Une fois les deux utilisateurs
      visibles, ils s'entendent dans les deux sens, avec attribution au bon nom
      et sans écho vers soi.
- [x] **Préférences pendant le churn.** Alice attribue à Bob un surnom local et
      un volume individuel. Bob passe dans Borealis puis revient dans Aurora :
      le surnom et le volume sont toujours présents après sa réintroduction.
- [x] **Coupure avant retrait.** Bob repart dans Borealis. Dès qu'il disparaît
      de la vue d'Alice, sa voix n'est plus audible ; aucun fragment tardif ne
      doit arriver après le retrait.
- [x] **Reconnexion et IDs déterministes.** Bob ferme puis reconnecte le client
      avec `bob@borealis` et le même certificat, avant de revenir dans Aurora.
      Les deux canaux gardent leur signification, aucun cache de canal ne migre
      vers l'autre realm, et le surnom ou volume local de Bob survit à la
      nouvelle session.
- [x] **Répétition sans churn anormal.** Répéter trois allers-retours
      Aurora/Borealis. Noter les notifications ou TTS de présence produites par
      le client ; aucune déconnexion réelle, UI bloquée, canal fantôme ou
      préférence attribuée au mauvais utilisateur n'est acceptable.
- [x] **Déconnexion propre.** Fermer Bob depuis n'importe quel realm : les vues
      restantes convergent et Alice continue à utiliser le serveur.

Réserves attendues en P6 :

- Ce scénario à deux realms est déterministe et local au serveur ; ce n'est pas
  encore un flavor métier de production. P7 fournit son contrat d'intégration et
  P8 porte le premier flavor Minecraft.
- Les ACL détaillées, messages texte, blobs, targets whisper et administration
  native restent refusés explicitement.
- Le certificat client fournit une identité de présentation, pas une
  authentification ni une autorisation. L'association à un principal arrive en
  P8.

---

## Signature

**OK.** Les dix cases ont été validées avec deux clients officiels Mumble, l'un
sur macOS et l'autre sur Windows.

- Version du serveur (commit) : `9b59527`
- Version des clients Mumble : non relevée
- Systèmes : macOS et Windows, versions non relevées
- Date : 2026-07-26
- Validé par : theking90000
- Résultat : **OK**
- Notes : arbres divergents et convergence à chaud sans reconnexion, isolation
  puis activation audio correctes, coupure avant retrait, préférences locales
  conservées pendant le churn et après reconnexion, aucune erreur client,
  déconnexion ou entité fantôme observée.

Le point de contrôle humain de P6 est validé. Avec les critères machine verts,
P6 est close.
