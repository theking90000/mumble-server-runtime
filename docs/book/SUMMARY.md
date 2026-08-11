# Summary

[Introduction](introduction.md)
[Boundaries](boundaries.md)

- [The model](model/index.md)
  - [Vocabulary](model/vocabulary.md)
  - [The shared view and scopes](model/scopes.md)
  - [Overlays](model/overlays.md)
  - [Audio as a relation](model/audio.md)
  - [Deltas and per-connection composition](model/deltas.md)
  - [Seeing the speaker](model/coupling.md)
  - [Cost](model/cost.md)
  - [Refusals](model/refusals.md)
- [Building an application](build/index.md)
  - [Running the arena](build/arena.md)
  - [Anatomy of an application](build/anatomy.md)
  - [The render function](build/render.md)
  - [Scope or overlay](build/scope-or-overlay.md)
  - [Declaring audio](build/audio.md)
  - [Client interactions](build/interactions.md)
  - [Migration between shards](build/migration.md)
  - [Configuration and limits](build/configuration.md)
  - [Load testing](build/load-testing.md)
- [Mumble compatibility](mumble/index.md)
  - [What a client can do](mumble/surface.md)
  - [The control plane](mumble/control-plane.md)
  - [The voice plane](mumble/voice-plane.md)
  - [Establishing compatibility](mumble/oracles.md)
- [Controller integration](controller/index.md)
  - [Getting started](controller/getting-started.md)
  - [Spaces](controller/spaces.md)
  - [Participants](controller/participants.md)
  - [Connecting a player to Mumble](controller/joining.md)
  - [A Minecraft plugin](controller/minecraft.md)
  - [Troubleshooting](controller/troubleshooting.md)
  - [Reference: the model](controller/how-it-works.md)
  - [Reference: lifecycles](controller/java.md)
  - [Reference: the Spaces Rust host](controller/server.md)
- [API documentation](reference/api.md)

<!--
Plan du livre. Une entrée est décommentée quand la page est écrite : avec
`create-missing = false`, une entrée sans fichier fait échouer la construction.

Un chapitre parent a besoin de son propre fichier : sans lui mdBook en fait un
« draft », affiché en grisé et non cliquable.

- [Development](dev/index.md)
  - [Building and testing](dev/building.md)
  - [Structural gates](dev/gates.md)
-->
