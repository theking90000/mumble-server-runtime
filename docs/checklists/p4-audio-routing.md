# Phase 4 — checklist du point de contrôle humain (routage audio deux clients)

> Roadmap P4, critère de « done » : _deux clients officiels s'entendent (humain) ;
> test de charge testkit — N clients simulés, débit soutenu, zéro perte interne,
> latence routeur bornée (CI) ; fallback TCP vérifié en coupant l'UDP._ Les
> critères machine étaient verts ; celui-ci l'est depuis le 2026-07-26 (voir
> « Signature »). Un agent ne peut pas juger si une voix est intelligible : il
> faut un humain, deux vrais clients et une oreille.

Ce que la CI prouve déjà (donc **hors** de cette checklist) :

- Le routeur pur : jamais de retour vers soi en parole normale, jamais de
  franchissement de domaine, accord strict entre la table précompilée et la
  politique, Opus identique octet pour octet, sur 4000 seeds
  (`voxloom-audio`, `tests/routing.rs`).
- Le relais réel entre deux clients en UDP, du tunnel TCP vers un auditeur UDP,
  de l'UDP vers un auditeur tunnelisé, et le refus d'une cible non enregistrée
  (`voxloom-server`, `tests/handshake_it.rs`).
- Quatre clients simulés, 25 rondes, 300 livraisons, **zéro perte interne**,
  chaque paquet vérifié par numéro de trame, charge utile et session émettrice,
  latence bornée (`voxloom-testkit`, `tests/voice_load.rs`).
- Le coût par destinataire du relais tient sous un plafond structurel
  (`ci/bench-audio.sh`).

Ce que **seul un humain** peut valider : que la voix qui traverse le routeur est
compréhensible, dans les deux sens, sur les deux transports.

---

## Pré-requis

- Deux clients officiels Mumble (1.5+ recommandé, Opus), idéalement sur deux
  machines ou deux comptes système, avec un casque pour éviter le larsen.
- Le binaire : `cargo build -p voxloom-server --release`.

## Lancement

```bash
./target/release/voxloom-server --tcp 0.0.0.0:64738
```

Se connecter à `127.0.0.1:64738` (pas `localhost`, qui résout d'abord en IPv6
que le bind `0.0.0.0` n'écoute pas). Certificat auto-signé à accepter ; il est
régénéré à chaque lancement du serveur, donc la demande revient à chaque
redémarrage. N'importe quel nom d'utilisateur convient (auth stub).

Garder le terminal du serveur visible : chaque refus est journalisé (cible non
enregistrée, budget épuisé, paquet hors bande de taille).

---

## Checklist (cocher, dater, signer)

- [x] **Présence.** Les deux clients se connectent et se voient dans la liste
      d'utilisateurs.
- [x] **Alice entend Bob.** Bob parle en mode normal : Alice l'entend, **voix
      comprise, sans hachage ni blancs**. Juger sur une phrase entière.
- [x] **Bob entend Alice.** Le sens inverse, même critère. Les deux sens sont à
      tester séparément : un routage cassé dans un seul sens est un cas réel.
- [x] **Pas d'écho.** En parlant, on ne s'entend **pas** soi-même revenir (hors
      loopback explicite). Un écho signifierait que le locuteur est routé vers
      lui-même.
- [x] **Attribution.** Le client indique le bon locuteur (l'icône de parole
      s'allume en face du bon nom, pas de l'autre).
- [x] **Loopback intact.** `Settings → Audio Output → Loopback → Server` : le
      loopback de P3 fonctionne toujours. C'est le test de non-régression de la
      phase précédente. Remettre sur `None` ensuite.
- [x] **Repli TCP d'un seul côté.** Cocher `Settings → Network → Force TCP mode`
      sur **Bob uniquement**, reconnecter. Alice reste en UDP. Les deux doivent
      continuer à s'entendre **dans les deux sens** : c'est le cas croisé, celui
      qui échoue si le serveur choisit le transport de l'émetteur au lieu de
      celui de chaque destinataire.
- [x] **Repli TCP des deux côtés.** Force TCP sur les deux clients : ils
      s'entendent toujours.
- [x] **Coupure d'UDP réelle** _(optionnel, plus fort que Force TCP)_ : bloquer
      l'UDP vers le port du serveur au pare-feu, **puis** connecter le client.
      Après ~20 s, il annonce la bascule (« UDP packets cannot be sent to or
      received from the server. Switching to TCP mode. ») et la voix passe par
      le tunnel, sans reconnexion.

      Bloquer l'UDP _pendant_ qu'un client est déjà connecté ne le fera **pas**
      basculer, et c'est le comportement du client officiel, pas un défaut du
      serveur : la bascule exige `(uiRemoteGood == 0 || uiGood == 0)`
      (`ServerHandler.cpp:653`), or ces compteurs sont cumulatifs et jamais
      remis à zéro (`CryptState.h:16`, `CryptStateOCB2.cpp:209`). Un client dont
      l'UDP a fonctionné une seule fois ne repassera donc jamais en TCP. Le
      serveur ne peut rien y faire : côté Murmur le transport par destinataire
      suit uniquement le transport du dernier paquet reçu de lui
      (`Server.cpp:975` / `:1721` / `:1037`), sans aucun timeout de liveness UDP.
- [x] **Déconnexion propre.** Fermer un client : l'autre le voit disparaître, et
      un nouveau client peut se connecter et être entendu.

Réserves connues (attendues en P4) :

- **Tout le monde entend tout le monde.** Le snapshot est trivial par
  construction ; il n'y a ni canaux séparés, ni proximité, ni permissions. C'est
  P6/P7/P8.
- **Shout et whisper ne font rien.** Ces cibles s'enregistrent par `VoiceTarget`,
  refusé jusqu'à P9. Le serveur journalise le refus au lieu de router.
- **Pas de positional audio spatialisé.** Les données de position traversent le
  relais, mais aucune politique de proximité ne les exploite (P8).

---

## Signature

**OK !!** Les dix cases sont validées sur deux vrais clients Mumble.

- Version du serveur (commit) : `dcf4916`
- Date : 2026-07-26
- Validé par : theking90000
- Notes / artefacts observés : voix comprise dans les deux sens, sans hachage ni
  blancs, sans écho, attribution correcte ; loopback P3 intact ; repli tunnel
  vérifié d'un seul côté (le cas croisé) puis des deux côtés ; déconnexion propre
  et reprise par un nouveau client. Un point a été investigué pendant le déroulé
  et **classé comportement normal du client officiel**, sans correctif serveur :
  bloquer l'UDP alors qu'un client est déjà connecté ne le fait jamais basculer
  en tunnel, parce que la bascule teste `uiGood`/`uiRemoteGood`, compteurs
  cumulatifs jamais remis à zéro (détail et références dans la case « Coupure
  d'UDP réelle » ci-dessus). Le scénario réel — pare-feu bloquant avant la
  connexion — bascule bien, au bout de ~20 s.

Le point de contrôle humain de P4 est **done**. Les critères machine (routeur
pur, relais deux clients sur les deux transports, test de charge sans perte,
coût par destinataire sous plafond) étaient déjà verts.
