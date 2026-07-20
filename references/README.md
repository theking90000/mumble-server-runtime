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

## À faire (Phase 0)

- [ ] Fixer le commit dans `references/mumble.pin` (**humain** : choisir une
      release stable du dépôt mumble-voip).
- [ ] Vendorer `Mumble.proto`, `MumbleUDP.proto` sous `references/vendored/`.
- [ ] Extraire les vecteurs OCB2 sous `references/vendored/ocb2-vectors/`.
