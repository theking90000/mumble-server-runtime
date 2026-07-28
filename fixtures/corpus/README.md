# fixtures/corpus/ — captures binaires réelles (zone vérificateur, R2)

> **Zone vérificateur (R2).** Modifiable uniquement par des tâches de vérification.
> Un diff qui touche ce répertoire ET une implémentation (`mumble-server-runtime-*/src`) est
> refusé par `ci/verifier-boundary.sh`.

Transcripts binaires de sessions réelles **client officiel ↔ Murmur**, enregistrés
via un proxy TCP/UDP qui ne décode rien : il journalise des octets horodatés,
annotés par direction. Ces captures sont l'oracle du codec (Phase 1) et de la
séquence de handshake (Phase 3).

## Format d'un scénario

Chaque scénario est un sous-répertoire `NN-nom/` contenant :

- `client-to-server.bin` / `server-to-client.bin` (ou un log entrelacé horodaté),
- `meta.json` : direction, horodatage, version client, version Murmur,
- `README.md` : description du scénario et ce qu'il est censé démontrer.

## Scénarios requis (≥ 8 pour clore la Phase 0)

- [x] `01-handshake` — handshake complet (Version, Authenticate, CryptSetup,
      ServerSync, CodecVersion, ServerConfig).
- [x] `02-channel-join-leave` — join puis leave d'un canal.
- [x] `03-channel-create-remove` — création puis suppression d'un canal.
- [x] `04-two-clients-talking` — deux clients qui parlent (audio dans les 2 sens).
- [-] `05-whisper` — whisper / voice target. : pas trouvé
- [x] `06-permission-denied` — action refusée par le serveur. : pas réussi
- [x] `07-disconnect` — déconnexion propre.
- [ ] `08-crypto-resync` — resync OCB2 si provocable.
- [ ] (`09-mumla-legacy-udp` — session client Android, **si** legacy UDP retenu,
      voir `docs/decisions/0001-legacy-udp.md`).

**Humain requis (R5/P0)** : installer Murmur + client officiel, dérouler ces
scénarios à la main (un agent ne pilote pas une GUI Qt). Le proxy d'enregistrement
est un livrable Phase 0 à écrire (trivial : il ne décode rien).
