# references/ — sources protocolaires vendored (R1)

La vérité protocolaire ne vient jamais de mémoire (R1). Elle vient d'ici.

## `references/mumble/` — clone pinné du dépôt mumble-voip

Le clone brut **n'est pas commité** (voir `.gitignore`). Ce qui est versionné :

- `references/mumble.pin` : le commit hash exact du dépôt mumble-voip de référence.
  Toute affirmation sur le wire format est traçable vers ce commit.
- les extraits explicitement vendorés sous `references/vendored/` : `Mumble.proto`,
  `MumbleUDP.proto`, et les vecteurs de test OCB2 de `CryptState` (les tests du
  dépôt officiel deviennent des fixtures crypto en Phase 1).

### Reproduire le clone localement

```bash
git clone https://github.com/mumble-voip/mumble references/mumble
git -C references/mumble checkout "$(cat references/mumble.pin)"
```

## Fait (Phase 0)

- [x] Commit fixé dans `references/mumble.pin` : release stable `v1.5.915`
      (`5fe5ec6e61b0c1cc414a8a8db548ec484eec6b90`).
- [x] `Mumble.proto`, `MumbleUDP.proto` vendorés sous `references/vendored/`.
- [x] Vecteurs OCB2 extraits sous `references/vendored/ocb2-vectors/`
      (`TestCrypt.cpp` + implémentation de référence). Provenance détaillée :
      `references/vendored/PROVENANCE.md`.
