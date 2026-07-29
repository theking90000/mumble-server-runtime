# Mumble compatibility

Unmodified Mumble clients connect to this server, on every platform the Mumble
project ships one for. No plugin, no custom build and no protocol extension is
involved.

That comes with a shape a client can feel. Everything a conventional server lets
a client change directly is refused here, because the voice session is rendered
from application state rather than edited. This chapter states what a client
gets as a result, and what backs the word compatible.

- [What a client can do](surface.md) covers the features that work, the ones
  that are refused, and the messages behind both.
- [The control plane](control-plane.md) follows a connection from the TLS
  handshake to a live session.
- [The voice plane](voice-plane.md) covers how speech travels, what is removed
  from it, and when a client falls back to the TCP tunnel.
- [Establishing compatibility](oracles.md) is the argument: what backs the
  claim, and what it does not cover.

[Boundaries](../boundaries.md) states the same limits as policy rather than as
consequence. No types or code appear in this chapter, which describes wire
behaviour rather than the interface an application programs against. That is
[Building an application](../build/index.md).
