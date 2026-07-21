# Provenance des extraits vendorés

Source : https://github.com/mumble-voip/mumble
Release : v1.5.915
Commit  : 5fe5ec6e61b0c1cc414a8a8db548ec484eec6b90 (voir `../mumble.pin`)

Reproduction :

```bash
git clone https://github.com/mumble-voip/mumble references/mumble
git -C references/mumble checkout "$(cat references/mumble.pin)"
```

## Fichiers

| Fichier vendoré | Chemin amont | Rôle |
|---|---|---|
| `Mumble.proto` | `src/Mumble.proto` | Messages TCP (control channel). Source prost. |
| `MumbleUDP.proto` | `src/MumbleUDP.proto` | Enveloppe UDP protobuf (1.5+). Source prost. |
| `ocb2-vectors/TestCrypt.cpp` | `src/tests/TestCrypt/TestCrypt.cpp` | Vecteurs OCB2 de référence (draft-krovetz-ocb-00) + tests des mitigations Mumble (xexstar, tamper, IV recovery). Deviennent des fixtures crypto en Phase 1. |
| `ocb2-vectors/CryptStateOCB2.cpp` | `src/crypto/CryptStateOCB2.cpp` | Implémentation OCB2 de référence. Lecture seule, pour tracer les mitigations. |
| `ocb2-vectors/CryptStateOCB2.h` | `src/crypto/CryptStateOCB2.h` | Idem, en-tête. |
| `ocb2-vectors/CryptState.h` | `src/crypto/CryptState.h` | Interface CryptState de base. |
| `protocol/MumbleProtocol.h` | `src/MumbleProtocol.h` | Enums `TCPMessageType` (0..26) et `UDPMessageType` (Audio=0, Ping=1), constantes (`MAX_UDP_PACKET_SIZE`, contextes audio, cibles réservées). Vérité des codes de type. |
| `protocol/Connection.cpp` | `src/Connection.cpp` | Chemin de lecture du framing TCP (`socketRead`) : en-tête 6 octets (`u16 BE` type + `u32 BE` longueur), limite `0x7fffff` (« huge packet » → drop). |
| `LICENSE` | `LICENSE` | Licence BSD du dépôt mumble-voip. Ces extraits en dépendent. |

Les `.cpp`/`.h` OCB2 sont des références de lecture (R1) : on les lit pour
comprendre une mitigation, puis on vérifie chaque affirmation contre eux avant
tout commentaire `// REF:`. Ils ne sont jamais compilés dans Voxloom.
