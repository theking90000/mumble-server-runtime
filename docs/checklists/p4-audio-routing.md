# Phase 4 — checklist du point de contrôle humain (routage audio deux clients)

> Roadmap P4, critère de « done » : _deux clients officiels s'entendent (humain) ;
> test de charge testkit — N clients simulés, débit soutenu, zéro perte interne,
> latence routeur bornée (CI) ; fallback TCP vérifié en coupant l'UDP._ Les
> critères machine sont verts ; celui-ci, non. Un agent ne peut pas juger si une
> voix est intelligible : il faut un humain, deux vrais clients et une oreille.

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

- [ ] **Présence.** Les deux clients se connectent et se voient dans la liste
      d'utilisateurs.
- [ ] **Alice entend Bob.** Bob parle en mode normal : Alice l'entend, **voix
      comprise, sans hachage ni blancs**. Juger sur une phrase entière.
- [ ] **Bob entend Alice.** Le sens inverse, même critère. Les deux sens sont à
      tester séparément : un routage cassé dans un seul sens est un cas réel.
- [ ] **Pas d'écho.** En parlant, on ne s'entend **pas** soi-même revenir (hors
      loopback explicite). Un écho signifierait que le locuteur est routé vers
      lui-même.
- [ ] **Attribution.** Le client indique le bon locuteur (l'icône de parole
      s'allume en face du bon nom, pas de l'autre).
- [ ] **Loopback intact.** `Settings → Audio Output → Loopback → Server` : le
      loopback de P3 fonctionne toujours. C'est le test de non-régression de la
      phase précédente. Remettre sur `None` ensuite.
- [ ] **Repli TCP d'un seul côté.** Cocher `Settings → Network → Force TCP mode`
      sur **Bob uniquement**, reconnecter. Alice reste en UDP. Les deux doivent
      continuer à s'entendre **dans les deux sens** : c'est le cas croisé, celui
      qui échoue si le serveur choisit le transport de l'émetteur au lieu de
      celui de chaque destinataire.
- [ ] **Repli TCP des deux côtés.** Force TCP sur les deux clients : ils
      s'entendent toujours.
- [ ] **Coupure d'UDP réelle** _(optionnel, plus fort que Force TCP)_ : bloquer
      l'UDP vers le port du serveur au pare-feu pendant que les clients sont
      connectés. Après la bascule annoncée par le client (« UDP packets cannot be
      sent to or received from the server. Switching to TCP mode. »), la voix
      doit continuer à passer par le tunnel, sans reconnexion.
- [ ] **Déconnexion propre.** Fermer un client : l'autre le voit disparaître, et
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

- Version du serveur (commit) : `__________`
- Versions des clients Mumble / OS : `__________`
- Date : `__________`
- Validé par : `__________`
- Notes / artefacts observés : `__________`

Tant que cette checklist n'est pas signée, le point de contrôle humain de P4
n'est **pas** « done » au sens de la roadmap, même si toute la CI est verte.
