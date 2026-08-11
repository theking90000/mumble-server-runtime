# Getting started

This page starts a controller server, registers one participant from Java, and
connects a real Mumble client to it.

## What you need

- **Java 8 or newer.** The SDK is compiled to Java 8 bytecode, so it runs on
  any Minecraft server from 1.8 onwards.
- **A Rust toolchain**, to build and run the controller server.
- **A Mumble client**, to check the result. Any recent official client.

## 1. Add the library

The SDK is published as a single artifact:

```text
be.theking90000.mumble:controller-spaces
```

Gradle (Kotlin DSL):

```kotlin
dependencies {
    implementation("be.theking90000.mumble:controller-spaces:0.1.0")
}
```

Gradle (Groovy DSL):

```groovy
dependencies {
    implementation 'be.theking90000.mumble:controller-spaces:0.1.0'
}
```

Maven:

```xml
<dependency>
  <groupId>be.theking90000.mumble</groupId>
  <artifactId>controller</artifactId>
  <version>0.1.0</version>
</dependency>
```

> **Work in progress.** No public release exists yet, so the coordinates above
> do not resolve from any repository. Until the first release, build the
> artifact locally with `./gradlew publishToMavenLocal` from
> `control-plane`, add `mavenLocal()` to your repositories, and depend
> on version `0.1.0-SNAPSHOT`.

The SDK brings gRPC and Protobuf with it. In a Minecraft plugin those must be
shaded into your jar and relocated, otherwise a second plugin shipping
different versions will break one of you. The configuration is in
[A Minecraft plugin](minecraft.md#shading).

## 2. Run the controller server

From the repository root:

```sh
cargo run -p mumble-controller-server -- --dev-self-signed
```

It prints where it is listening:

```text
mumble-controller-server: Controller listening on 127.0.0.1:4000, Mumble listening on 0.0.0.0:64738
```

Two ports for two audiences. Port `4000` is the gRPC port your Java code talks
to. Port `64738` is the standard Mumble port that players connect to.

`--dev-self-signed` generates a throwaway TLS certificate on every start, and
is for local development only. Anywhere else, pass a real certificate with
`--mumble-cert` and `--mumble-key`. The full option list is on the
[Rust server](server.md) reference page.

## 3. Your first session

```java
import be.theking90000.mumble.controller.*;
import java.net.URI;

public class VoiceDemo {
    public static void main(String[] args) throws Exception {
        ControllerSession session = ControllerSession.builder(
                        ControllerId.of("my-server"),
                        URI.create("http://127.0.0.1:4000"))
                .build();

        session.start().get();

        ParticipantHandle steve = session.registerParticipant(
                ParticipantId.of("steve"),
                ParticipantSpec.builder(SpaceKey.of("lobby"), "Steve").build());

        String token = steve.whenMumbleJoinTokenAvailable().get().value();
        System.out.println("mumble://steve:" + token + "@127.0.0.1");

        Thread.sleep(600_000L);
        session.stop().get();
    }
}
```

A few points about that code:

- `ControllerId` names your application. It is not a password, so use something
  stable such as your plugin name.
- The endpoint scheme is `http` because the controller port is plaintext in v1.
  Keep the controller server on the same machine as your application. The SDK
  also accepts an `https` endpoint with `.tls(TlsConfig.systemTrust())`, for a
  deployment that terminates TLS in front of the server.
- `start()` returns a future that completes once the runtime has accepted the
  session and reconciled its initial state.
- `registerParticipant` declares that Steve exists, belongs in the `lobby`
  Space and is displayed as `Steve`. It returns immediately, and ownership is
  granted asynchronously.
- `whenMumbleJoinTokenAvailable()` yields the password Steve's Mumble client
  needs. See [Connecting a player to Mumble](joining.md).

## 4. Connect

Run the program, copy the `mumble://` line it prints, and open it. Mumble
launches and connects. The certificate warning is expected with
`--dev-self-signed`.

The channel you arrive in is named after the Space, and your name in it is the
display name from the spec. Register a second participant under a different
identifier, open its URL in another Mumble client, and the two can talk.

## Registering before the session is connected

`start()` is not a prerequisite for describing state. Participants and
observations registered beforehand are folded into the opening snapshot:

```java
ControllerSession session = ControllerSession.builder(id, endpoint).build();
session.registerParticipant(alice, aliceSpec);
session.registerParticipant(bob, bobSpec);
session.start();
```

A plugin needs this, because `onEnable` may want to restore state before the
connection is up, and because players can join while the session is still
connecting.

Blocking on `start().get()` is fine in a `main` method and wrong on a game
server's main thread. [A Minecraft plugin](minecraft.md) uses the non-blocking
form.
