/**
 * Declarative Java client for Mumble Controller.
 *
 * <p>A {@link be.theking90000.mumble.controller.ControllerSession} owns the complete desired
 * state declared by one controller instance. Participants and explicit space observations may be
 * registered before the session starts; they are then included in the initial reconciliation
 * snapshot. The session renews its lease and reconnects automatically until it is stopped or
 * encounters a permanent failure.</p>
 *
 * <p>A {@link be.theking90000.mumble.controller.ParticipantHandle} is a revocable local
 * registration. Its {@link be.theking90000.mumble.controller.ParticipantSpec} is desired state,
 * while {@link be.theking90000.mumble.controller.ParticipantStatus} reports the latest state seen
 * by the runtime. Placement is part of the participant spec; there is no separate move command.
 * Ownership capabilities, protocol revisions, session epochs, and retry correlation identifiers
 * remain internal to this package.</p>
 *
 * <p>Space data is read-only. Each {@link
 * be.theking90000.mumble.controller.SpaceSnapshot} is a full immutable replacement identified by
 * both a semantic {@link be.theking90000.mumble.controller.SpaceKey} and an opaque {@link
 * be.theking90000.mumble.controller.SpaceIncarnation}. Explicit observation is persistent desired
 * state. A one-shot fetch does not add an observation. The runtime may also send spaces that contain
 * participants owned by the session.</p>
 *
 * <p>Receipt, application, and Mumble publication are separate milestones. Completion of a write
 * future means that the runtime accepted the corresponding client revision. Callers that need to
 * observe application or publication must inspect the returned watermarks and subsequent status or
 * space snapshots.</p>
 *
 * <p>All public objects are thread-safe. Listener invocations are serialized on the callback
 * executor configured on the session builder. A listener exception is isolated and does not stop
 * transport processing or other listeners.</p>
 *
 * <p>Typical setup:</p>
 *
 * <pre>{@code
 * Executor callbacks = Executors.newSingleThreadExecutor();
 * ControllerSession session = ControllerSession.builder(
 *         ControllerId.of("lobby-11"),
 *         URI.create("https://voice.example.net:4000"))
 *     .tls(TlsConfig.systemTrust())
 *     .callbackExecutor(callbacks)
 *     .build();
 *
 * ParticipantHandle player = session.registerParticipant(
 *         ParticipantId.of(playerUuid.toString()),
 *         ParticipantSpec.builder(SpaceKey.of("lobby"), "Alex").build());
 *
 * session.start().thenCompose(ignored -> player.whenOwned());
 * }</pre>
 */
package be.theking90000.mumble.controller;
