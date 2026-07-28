# Configuration and limits

There is no configuration file and no environment variable. Everything is Rust
code, and the values that are not code are constants rather than settings.

The reason is in [Boundaries](../boundaries.md): everything an application would
want to vary at runtime, names, trees, who hears whom, belongs to the render.
What is left is the handful of values the Mumble handshake demands before any
view exists.

## `GatewayConfig`

```rust
let config = GatewayConfig {
    bind: "0.0.0.0:64738".parse()?,
    welcome_text: "Arena. Double-click a channel to choose.".to_owned(),
    max_users: 500,
    ..GatewayConfig::default()
};
```

| field | default | effect |
| --- | --- | --- |
| `bind` | `0.0.0.0:64738` | TLS control plane. UDP binds the same port |
| `welcome_text` | empty | sent in `ServerSync`. Empty omits the field |
| `max_bandwidth` | `72_000` | advertised bits per second |
| `max_users` | `100` | advertised, and the admission ceiling |
| `recording_allowed` | `true` | advisory |
| `allow_html` | `true` | whether HTML is allowed in text |
| `message_length` | `5_000` | maximum text message length |
| `version` | `(1, 5, 0)` | advertised version |

Two of these are advertised rather than enforced. The bandwidth ceiling is sent
to clients and not applied server-side. The recording flag is a request the
client is free to ignore.

`max_users` is the exception among the advertised values: connections past it
are refused, rather than admitted into a runtime that announced a smaller
number.

The default version is 1.5.0 because the protobuf UDP format was introduced
there, and a real client negotiates it on that basis.

## The certificate

```rust
let identity = Identity::self_signed(vec!["localhost".to_owned()])?;
```

Self-signed generation exists for local development. A deployment supplies a
persistent certificate instead. Clients key their local preferences on the
certificate hash, so a fresh certificate at every restart looks like a new
server each time.

A client certificate is requested but not required. When present it proves
identity rather than authority, and its lowercase SHA-1 hash reaches the router
in `ConnectionIdentity`. What that identity is worth is the application's
decision.

TLS 1.2 is pinned. A widely deployed client build crashes on a TLS 1.3 session.

## Limits that are not settable

Each of these has a cost behind it rather than a preference.

| limit | value | consequence of exceeding |
| --- | --- | --- |
| scope depth | 4 segments | the render is refused |
| scopes observed per connection | 4 | the render is refused |
| context actions per connection | 64 | the render is refused |
| delta history retained | 256 versions | the connection is closed |
| output queue per connection | 1024 messages | the connection is closed |
| minimum interval between turns | 50 ms | turns are coalesced |

The two closures are worth distinguishing from the three refusals. A refused
render keeps the previous view and closes nothing, because the fault is in the
application's own code and the committed view is still correct. A connection
falling off the tail of the journal or overflowing its queue has no correct view
left to keep, and closing it is the only alternative to letting it drift.

Running out of scopes is a modelling signal rather than a configuration problem.
See [Scope or overlay](scope-or-overlay.md).

## Data-plane limits

| limit | value |
| --- | --- |
| maximum voice datagram | 1024 bytes |
| minimum voice datagram | 2 bytes |
| sustained voice packets per connection | 200 per second |
| voice burst allowance | 400 packets |
| text messages per connection | 1 per second, burst of 5 |

A client at the usual 10 ms framing emits 100 packets per second, so the voice
budget leaves headroom for a faster codec setting while bounding a flood to
twice the legitimate worst case. A connection exceeding it has packets dropped
rather than being disconnected.

The text allowance is the same one the reference server applies. A flood refused
at the socket never crosses a mailbox nor wakes a shard.

The datagram bounds match the reference server exactly, which keeps tunnelled
and direct audio on one rule.

## Two entry points

`serve` covers the common case:

```rust
serve(config, identity, |runtime| {
    let lobby = runtime.create_shard(|_handle| Lobby::new());
    AlwaysAttach(lobby.shard())
})
.await
```

`Gateway::bind` followed by `Gateway::serve` splits the two, which is what a
test binding an ephemeral port needs in order to read the address back:

```rust
let gateway = Gateway::bind(config, identity).await?;
let address = gateway.address();
gateway.serve(router).await
```
