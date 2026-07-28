# Provenance of vendored excerpts

Source: https://github.com/mumble-voip/mumble
Release: v1.5.915
Commit: 5fe5ec6e61b0c1cc414a8a8db548ec484eec6b90 (see `../mumble.pin`)

Reproduction:

```bash
git clone https://github.com/mumble-voip/mumble references/mumble
git -C references/mumble checkout "$(cat references/mumble.pin)"
```

## Files

| Vendored file                     | Upstream path                       | Role                                                                                                                                                                        |
| --------------------------------- | ----------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Mumble.proto`                    | `src/Mumble.proto`                  | TCP messages (control channel). Prost source.                                                                                                                               |
| `MumbleUDP.proto`                 | `src/MumbleUDP.proto`               | UDP protobuf envelope (1.5+). Prost source.                                                                                                                                 |
| `ocb2-vectors/TestCrypt.cpp`      | `src/tests/TestCrypt/TestCrypt.cpp` | Reference OCB2 vectors (draft-krovetz-ocb-00) + tests of Mumble mitigations (xexstar, tamper, IV recovery). They become crypto fixtures in Phase 1.                         |
| `ocb2-vectors/CryptStateOCB2.cpp` | `src/crypto/CryptStateOCB2.cpp`     | Reference OCB2 implementation. Read-only, used to trace mitigations.                                                                                                        |
| `ocb2-vectors/CryptStateOCB2.h`   | `src/crypto/CryptStateOCB2.h`       | Same, header.                                                                                                                                                               |
| `ocb2-vectors/CryptState.h`       | `src/crypto/CryptState.h`           | Base CryptState interface.                                                                                                                                                  |
| `protocol/MumbleProtocol.h`       | `src/MumbleProtocol.h`              | `TCPMessageType` enums (0..26) and `UDPMessageType` (Audio=0, Ping=1), constants (`MAX_UDP_PACKET_SIZE`, audio contexts, reserved targets). Source of truth for type codes. |
| `protocol/Connection.cpp`         | `src/Connection.cpp`                | TCP framing read path (`socketRead`): 6-byte header (`u16 BE` type + `u32 BE` length), limit `0x7fffff` (“huge packet” → drop).                                             |
| `LICENSE`                         | `LICENSE`                           | BSD license from the mumble-voip repository. These excerpts depend on it.                                                                                                   |

The OCB2 `.cpp`/`.h` files are read-only references (R1): they are read to
understand a mitigation, then every claim is checked against them before any
`// REF:` comment. They are never compiled in Voxloom.
