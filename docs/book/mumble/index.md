# Mumble compatibility

What an unmodified Mumble client gets from this server, and why the claim of
compatibility is checkable rather than asserted.

[Boundaries](../boundaries.md) states the policy: nothing is driven from the
client, and the voice state comes from the render alone. This chapter states the
consequence a client actually observes, message by message.

- [Supported surface](surface.md) lists what a client may send, what it is
  answered, and what the server never sends.
- [The control plane](control-plane.md) covers the sequence from a TCP accept to
  a usable session, and the transport decisions behind it.
- [The voice plane](voice-plane.md) covers how audio travels, what is stripped
  from it, and when a connection falls back to the TCP tunnel.
- [Establishing compatibility](oracles.md) describes the mechanisms that judge
  the implementation, and states what none of them proves.

No types and no code appear in this chapter. It describes wire behaviour, not
the interface an application programs against, which is
[Building an application](../build/index.md).
