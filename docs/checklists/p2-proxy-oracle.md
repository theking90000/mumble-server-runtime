# Phase 2 — checklist du point de contrôle humain (proxy MITM oracle)

> Roadmap P2, critère de « done » : _appel vocal complet à travers le proxy sans
> artefact audible ; transcript du proxy identique en structure aux captures de
> référence ; checklist humaine signée dans le dépôt._ Un agent ne peut pas juger
> cet oracle : il faut un humain, un vrai client Mumble et une oreille. Cette
> checklist est ce que l'humain déroule et signe.

Ce que la CI prouve déjà (donc **hors** de cette checklist) :

- le codec encode/décode tout le corpus (P1, `corpus-decode`) ;
- le contrôle traverse le proxy décodé/ré-encodé, `CryptSetup` réécrit, resyncs
  absorbés (`tests/tcp_plane.rs`) ;
- la ré-encryption UDP est sans perte sur le corpus réel, 0 rejet
  (`tests/corpus_reencrypt.rs`) ;
- le relais UDP asynchrone corrèle l'adresse source à la session et ré-encrypte
  la voix dans les deux sens sur de vraies sockets (`tests/udp_plane.rs`).

Ce que **seul un humain** peut valider : que le client officiel se comporte
normalement de bout en bout, voix comprise.

---

## Pré-requis

- Un Murmur réel qui tourne (idéalement 1.5.x, le corpus 03–07 est un 1.5.857).
- Le client officiel Mumble.
- Le binaire : `cargo build -p voxloom-mitm-proxy --release`.

## Lancement

Le proxy écoute le même port en TCP (contrôle) et UDP (voix), comme Mumble.
Pointe-le sur le vrai Murmur (sur un autre port / une autre machine) :

```bash
# Murmur écoute p.ex. 127.0.0.1:64739 ; le proxy prend 64738 pour le client.
voxloom-mitm-proxy --listen 0.0.0.0:64738 --upstream 127.0.0.1:64739
```

Dans le client Mumble, connecte-toi à `localhost:64738` (le proxy), **pas** au
Murmur directement. Accepte le certificat auto-signé présenté par le proxy.

---

## Checklist (cocher, dater, signer)

- [x] **Connexion.** Le client se connecte à travers le proxy et atteint l'état
      connecté (arbre des canaux, liste d'utilisateurs, soi-même présent).
- [x] **Contrôle.** Rejoindre / quitter un canal, créer / supprimer un canal :
      chaque action se reflète correctement dans le client.
- [x] **UDP négocié.** Le client indique le mode UDP actif (pas de repli tunnel
      TCP permanent). Dans Mumble : l'info de connexion montre UDP, pas « TCP ».
- [x] **Voix aller.** En parlant, un second client (ou la boucle « test audio »
      du serveur) reçoit la voix **sans artefact audible** (pas de hachage, pas
      de blancs, pas de latence anormale au-delà du réseau).
- [x] **Voix retour.** La voix de l'autre sens arrive de même, sans artefact.
- [x] **Deux clients.** Deux clients réels s'entendent mutuellement à travers le
      proxy (rejoue le scénario `04-two-clients-talking`).
- [x] **Resync crypto.** Couper puis rétablir l'UDP (p.ex. réseau qui bouge, ou
      forcer un resync) : la voix reprend, le proxy n'a pas planté ni bouclé sur
      des rejets. Les resyncs sont absorbés côté proxy (attendu, cf. `session.rs`).
- [x] **Déconnexion propre.** Fermer le client : le proxy libère la session
      (aucune erreur bruyante persistante), un nouveau client peut se reconnecter.
- [x] **Transcript.** Comparer la structure des messages vus par le proxy (logs)
      à un scénario de référence du corpus : même séquence de handshake, pas de
      message de contrôle inattendu.

Réserve connue héritée de P0 : `05-whisper` et `06-permission-denied` peuvent ne
pas exercer réellement le comportement voulu (à revérifier pour les golden tests
de P3) — sans effet sur la validation voix ci-dessus.

---

## Signature

- Version du proxy (commit) : `2fb1e98`
- Version de Murmur / du client : Murmur 1.5.857 / Mumble 1.5.901 (macOS) + Windows
- Date : `2026-07-24`
- Validé par : theking90000 (martin.cogh@gmail.com)
- Notes / artefacts observés : appel vocal complet à travers le proxy sans
  artefact audible, dans les deux sens, deux clients, resync et déconnexion
  propre — OK. Piège macOS relevé et corrigé (commit `2fb1e98`) : le client
  Mumble macOS (Qt/OpenSSL) segfault à la fin du handshake si le proxy négocie
  TLS 1.3 ; le proxy est désormais épinglé sur TLS 1.2 (comme Murmur), plus de
  crash. Se connecter au proxy via `127.0.0.1:64738` (pas `localhost`, qui
  résout d'abord en IPv6 non écouté).

Tant que cette checklist n'est pas signée, P2 n'est **pas** « done » au sens de
la roadmap, même si toute la CI est verte.
