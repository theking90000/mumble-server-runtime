# mumble-server-runtime-recording-proxy

Proxy TCP/UDP d'enregistrement du corpus (livrable Phase 0 #3). Il capture des
sessions **client Mumble officiel ↔ Murmur** dans des fichiers `.voxcap`.

Il **ne décode pas** le protocole : il termine la TLS de contrôle pour journaliser
le flux en clair, et relaie l'UDP voix en aveugle (OCB2 non déchiffré). Le décodage
est l'affaire de la Phase 1.

## Comment capturer un scénario

1. Lancer Murmur sur un port distinct de celui du proxy, p. ex. `64739` :

   ```ini
   # murmur.ini
   port=64739
   ```

2. Lancer le proxy (écoute le client sur 64738, relaie vers Murmur sur 64739) :

   ```bash
   cargo run -p mumble-server-runtime-recording-proxy -- record \
     --listen 0.0.0.0:64738 \
     --upstream 127.0.0.1:64739 \
     --scenario 01-handshake \
     --out fixtures/corpus/01-handshake
   ```

3. Connecter le **client Mumble officiel** à `localhost:64738` (accepter le
   certificat auto-signé présenté par le proxy), dérouler le scénario, puis
   `Ctrl-C` sur le proxy pour clore la capture.

Chaque run produit `session.voxcap` (octets bruts horodatés, annotés par
direction/transport) et `meta.json` (scénario, adresses, totaux) dans `--out`.

## Inspecter une capture

```bash
cargo run -p mumble-server-runtime-recording-proxy -- dump fixtures/corpus/01-handshake/session.voxcap --hex
```

## Notes

- Le proxy présente un certificat **auto-signé** au client et **accepte
  n'importe quel** certificat de Murmur : c'est acceptable car c'est un outil de
  capture local, jamais un composant du runtime.
- Plusieurs clients simultanés sont gérés (chaque adresse source UDP obtient sa
  propre socket montante), ce qui permet le scénario « deux clients qui parlent ».
- Le contrôle TCP est journalisé **en clair** (dont `CryptSetup` et ses clés) ;
  la Phase 1 pourra déchiffrer l'UDP OCB2 capturé à partir de ces clés.
