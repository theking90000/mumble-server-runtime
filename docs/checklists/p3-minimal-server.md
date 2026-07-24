# Phase 3 — checklist du point de contrôle humain (serveur minimal)

> Roadmap P3, critère de « done » : _client officiel connecté, loopback
> fonctionnel (humain) ; le client simulé rejoue le handshake sans panique (CI) ;
> deux clients simulés connectés simultanément avec vues indépendantes (CI)._ Les
> deux derniers points sont prouvés en CI ; celui-ci, non. Un agent ne peut pas
> piloter une GUI Qt ni juger l'audio : il faut un humain, un vrai client Mumble
> et une oreille. Cette checklist est ce que l'humain déroule et signe.

Ce que la CI prouve déjà (donc **hors** de cette checklist) :

- `build_handshake` émet l'ordre exact de Murmur, invariants §20 d'ordonnancement
  testés dessus (`voxloom-server`, tests unitaires `handshake`).
- Un vrai client TLS déroule le handshake de bout en bout, associe l'UDP par
  preuve cryptographique, et le loopback (target 31) revient déchiffré, avec la
  session estampillée et l'Opus préservé — en UDP **et** en tunnel TCP
  (`voxloom-server`, `tests/handshake_it.rs`).
- Le `SimulatedMumbleClient` (juge strict §20) rejoue le handshake du vrai
  serveur **sans aucune violation**, et deux clients simulés connectés en même
  temps ont des vues indépendantes contenant les deux utilisateurs
  (`voxloom-testkit`, `tests/handshake_conformance.rs`).

Ce que **seul un humain** peut valider : que le client officiel Mumble se connecte
sans Murmur et entende le loopback serveur, voix comprise.

---

## Pré-requis

- Le client officiel Mumble (1.5+ recommandé, Opus).
- Le binaire : `cargo build -p voxloom-server --release`.

## Lancement

Le serveur écoute le même port en TCP (contrôle) et UDP (voix), comme Mumble.

```bash
voxloom-server --tcp 0.0.0.0:64738
```

Dans le client Mumble, connecte-toi à `127.0.0.1:64738` (pas `localhost`, qui
résout d'abord en IPv6 `::1` que le bind `0.0.0.0` n'écoute pas — même piège
qu'en P2). Accepte le certificat auto-signé. N'importe quel nom d'utilisateur /
mot de passe est accepté (auth stub en P3).

Pour tester le loopback : dans le client Mumble, **Configuration → Audio →
Loopback → « Serveur »** (Settings → Audio Output → Loopback → Server), puis
parle. La voix doit revenir depuis le serveur.

---

## Checklist (cocher, dater, signer)

- [ ] **Connexion.** Le client se connecte au serveur (sans Murmur) et atteint
      l'état connecté : canal racine visible, soi-même présent dans la liste.
- [ ] **Handshake propre.** Aucun message d'erreur ni « Reject » ; le client
      affiche le nom du serveur et le texte de bienvenue.
- [ ] **UDP négocié.** L'info de connexion du client montre **UDP** actif (le
      ping UDP chiffré passe), pas un repli permanent en tunnel TCP.
- [ ] **Loopback serveur (UDP).** Mode loopback « Serveur », en parlant on
      s'entend soi-même revenir **sans artefact audible** (pas de hachage, pas de
      blancs, latence seulement celle du réseau local).
- [ ] **Loopback en tunnel TCP.** Forcer le mode « Force TCP » dans le client
      (Configuration → Réseau), refaire le test loopback : la voix revient
      toujours (repli tunnel TCP fonctionnel).
- [ ] **Deux clients.** Deux clients réels connectés simultanément se voient
      mutuellement dans la liste d'utilisateurs (présence diffusée). _(Le routage
      voix entre eux est P4 ; ici on ne valide que la présence + le loopback de
      chacun.)_
- [ ] **Déconnexion propre.** Fermer un client : l'autre voit l'utilisateur
      disparaître (UserRemove) ; un nouveau client peut se reconnecter.

Réserve connue (attendue en P3) : parler en mode **normal** (target 0), sans le
loopback, ne produit aucun retour — il n'y a pas encore de graphe de routage
audio (P4). Seul le loopback serveur (target 31) est réfléchi.

---

## Signature

- Version du serveur (commit) : `__________`
- Version du client Mumble / OS : `__________`
- Date : `__________`
- Validé par : `__________`
- Notes / artefacts observés : `__________`

Tant que cette checklist n'est pas signée, le point de contrôle humain de P3
n'est **pas** « done » au sens de la roadmap, même si toute la CI est verte. Les
deux critères machine (client simulé sans panique, deux clients simulés
indépendants) sont, eux, déjà verts.
