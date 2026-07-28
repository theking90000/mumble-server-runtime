# references/ — vendored protocol sources (R1)

Protocol truth is documented here.

## `references/mumble/` — pinned clone of the mumble-voip repository

The raw clone is not committed (see `.gitignore`). What is versioned:

- `references/mumble.pin`: the exact commit hash of the reference mumble-voip repository.
  Any claim about the wire format is traceable to this commit.
- the excerpts explicitly vendored under `references/vendored/`: `Mumble.proto`,
  `MumbleUDP.proto`, and the OCB2 test vectors for `CryptState` (the official
  repository tests become crypto fixtures in Phase 1).

### Reproduce the clone locally

```bash
git clone https://github.com/mumble-voip/mumble references/mumble
git -C references/mumble checkout "$(cat references/mumble.pin)"
```
