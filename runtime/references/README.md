# Vendored protocol sources

Protocol truth. Every claim about the Mumble wire format traces back here,
pinned to the exact upstream commit recorded in `mumble.pin`.

What is kept in version control lives under `vendored/`: `Mumble.proto` and
`MumbleUDP.proto`, the OCB2 test vectors for `CryptState`, and the protocol
sources describing framing and the connection lifecycle. Upstream copyright and
license are retained, in `vendored/LICENSE` and `vendored/PROVENANCE.md`. The
OCB2 vectors are consumed directly as golden fixtures by the crypto crate's
tests.

The full clone under `mumble/` is excluded by `.gitignore`. To reproduce it:

```bash
git clone https://github.com/mumble-voip/mumble runtime/references/mumble
git -C runtime/references/mumble checkout "$(cat runtime/references/mumble.pin)"
```
