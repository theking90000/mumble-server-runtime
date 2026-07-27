//! The deterministic Aurora/Borealis flavor, as a flavor.
//!
//! This crate owns the business model Phase 6 kept inside the server: two
//! realms, who belongs to which, and the rule that a member sees and hears only
//! its own realm. It reaches Voxloom exclusively through the public contract of
//! `voxloom-flavor`, which is what makes it a proof that the contract is
//! sufficient rather than a convenience for the runtime.
//!
//! What stays here and never leaks into a central crate: realms, membership,
//! the `@aurora`/`@borealis` naming convention, and every mutation of them.
//! What Voxloom gets back: semantic views and receiver-owned routes.
//!
//! REF: docs/voxloom-roadmap-agents-v0_1.md P7 T6
//! REF: docs/decisions/0002-flavor-owns-business-state.md
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use voxloom_flavor::{
    ChannelKey, ConnectionId, DesiredAudioRoute, DesiredChannel, DesiredClientView, DesiredUser,
    FlavorError, FlavorRevision, InteractionRegistry, PermissionBits, RenderOutput, SemanticKey,
    ServerPresentation, SnapshotSource, UserKey, VoiceEvent, VoiceFlavor,
};

const AURORA_KEY: &str = "realm:aurora";
const BOREALIS_KEY: &str = "realm:borealis";

/// One of the two deterministic realms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Realm {
    Aurora,
    Borealis,
}

impl Realm {
    pub const ALL: [Realm; 2] = [Realm::Aurora, Realm::Borealis];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Realm::Aurora => "Aurora",
            Realm::Borealis => "Borealis",
        }
    }

    /// The semantic key Voxloom will resolve to a per-connection channel id.
    #[must_use]
    pub fn key(self) -> ChannelKey {
        ChannelKey(SemanticKey::Static(
            match self {
                Realm::Aurora => AURORA_KEY,
                Realm::Borealis => BOREALIS_KEY,
            }
            .to_owned(),
        ))
    }

    /// The realm a channel key denotes, if it denotes one at all.
    #[must_use]
    pub fn from_key(key: &ChannelKey) -> Option<Realm> {
        match &key.0 {
            SemanticKey::Static(value) if value == AURORA_KEY => Some(Realm::Aurora),
            SemanticKey::Static(value) if value == BOREALIS_KEY => Some(Realm::Borealis),
            _ => None,
        }
    }

    const fn sort_order(self) -> i32 {
        match self {
            Realm::Aurora => 0,
            Realm::Borealis => 1,
        }
    }
}

/// One connection as this flavor knows it: a display identity and a realm.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Member {
    name: String,
    certificate_hash: Option<String>,
    realm: Realm,
}

/// An immutable membership snapshot. Voxloom treats it as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    revision: FlavorRevision,
    members: BTreeMap<ConnectionId, Member>,
    root_name: String,
    presentation: ServerPresentation,
}

impl Snapshot {
    #[must_use]
    pub const fn revision(&self) -> FlavorRevision {
        self.revision
    }

    /// The realm a connection currently belongs to, for the integration's own
    /// observability. Voxloom never asks this.
    #[must_use]
    pub fn realm_of(&self, connection: ConnectionId) -> Option<Realm> {
        self.members.get(&connection).map(|member| member.realm)
    }
}

/// The mutable business state, owned entirely by this flavor.
#[derive(Debug)]
struct World {
    revision: u64,
    members: BTreeMap<ConnectionId, Member>,
}

/// The reference flavor.
///
/// Its concurrency model is its own business (spec 24.2): a mutex held for
/// map updates only, never across a render, and never visible to Voxloom.
#[derive(Debug)]
pub struct ReferenceFlavor {
    world: Mutex<World>,
    root_name: String,
    presentation: ServerPresentation,
}

impl ReferenceFlavor {
    /// A flavor with no member yet.
    ///
    /// `root_name` and `presentation` are what the composed application wants
    /// clients to see; they are constants of this flavor, not runtime state.
    #[must_use]
    pub fn new(root_name: impl Into<String>, presentation: ServerPresentation) -> Self {
        Self {
            world: Mutex::new(World {
                revision: 0,
                members: BTreeMap::new(),
            }),
            root_name: root_name.into(),
            presentation,
        }
    }

    /// Lock the business state, recovering a poisoned mutex.
    ///
    /// The guarded data is a plain map with no invariant that a panic could
    /// leave half-applied, so refusing to serve every later connection would be
    /// a worse answer than continuing.
    fn world(&self) -> std::sync::MutexGuard<'_, World> {
        match self.world.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn admit(&self, connection: ConnectionId, name: &str, certificate_hash: Option<String>) {
        let (name, realm) = identity(name);
        let member = Member {
            name,
            certificate_hash,
            realm,
        };
        let mut world = self.world();
        if world.members.get(&connection) == Some(&member) {
            return;
        }
        world.members.insert(connection, member);
        bump(&mut world);
    }

    fn forget(&self, connection: ConnectionId) {
        let mut world = self.world();
        if world.members.remove(&connection).is_some() {
            bump(&mut world);
        }
    }

    fn move_to_realm(&self, connection: ConnectionId, realm: Realm) {
        let mut world = self.world();
        let Some(member) = world.members.get_mut(&connection) else {
            return;
        };
        if member.realm == realm {
            return;
        }
        member.realm = realm;
        bump(&mut world);
    }
}

/// Advance the business revision. Saturating: a stuck revision makes the
/// integration keep publishing the same one, it never rewinds.
fn bump(world: &mut World) {
    world.revision = world.revision.saturating_add(1);
}

impl SnapshotSource for ReferenceFlavor {
    /// Take an immutable copy of the current membership.
    ///
    /// The result is a value: mutations that happen afterwards cannot change a
    /// snapshot Voxloom is already rendering.
    fn snapshot(&self) -> Arc<Snapshot> {
        let world = self.world();
        Arc::new(Snapshot {
            revision: FlavorRevision::new(world.revision),
            members: world.members.clone(),
            root_name: self.root_name.clone(),
            presentation: self.presentation.clone(),
        })
    }
}

impl VoiceFlavor for ReferenceFlavor {
    type Snapshot = Snapshot;

    fn revision(&self, snapshot: &Self::Snapshot) -> FlavorRevision {
        snapshot.revision
    }

    fn render(
        &self,
        snapshot: &Self::Snapshot,
        connection: ConnectionId,
    ) -> Result<RenderOutput, FlavorError> {
        let viewer = snapshot.members.get(&connection).ok_or_else(|| {
            FlavorError::render_refused(connection, "connection is not a member of this snapshot")
        })?;

        let mut view = DesiredClientView::empty();
        let root = view.root_channel.clone();
        if let Some(channel) = view.channels.get_mut(&root) {
            channel.name = snapshot.root_name.clone();
        }
        for realm in Realm::ALL {
            // The label is viewer-relative on purpose: two members in different
            // realms must hold observably different trees.
            let relation = if realm == viewer.realm {
                "Your realm"
            } else {
                "Switch to"
            };
            let key = realm.key();
            view.channels.insert(
                key.clone(),
                DesiredChannel {
                    key: key.clone(),
                    parent: root.clone(),
                    name: format!("{relation} · {}", realm.label()),
                    description: None,
                    sort_order: realm.sort_order(),
                    temporary: false,
                    max_users: None,
                    enter_restricted: false,
                    can_enter: true,
                    links: BTreeSet::new(),
                },
            );
        }

        for (member_connection, member) in &snapshot.members {
            if member.realm != viewer.realm {
                continue;
            }
            let key = UserKey(SemanticKey::Dynamic(member_connection.get()));
            view.users.insert(
                key.clone(),
                DesiredUser {
                    key,
                    source_connection: Some(*member_connection),
                    name: member.name.clone(),
                    channel: member.realm.key(),
                    user_id: None,
                    certificate_hash: member.certificate_hash.clone(),
                    mute: false,
                    deaf: false,
                    suppress: false,
                    self_mute: false,
                    self_deaf: false,
                    priority_speaker: false,
                    recording: false,
                    comment: None,
                    texture: None,
                },
            );
        }

        let effective = PermissionBits(
            PermissionBits::TRAVERSE
                | PermissionBits::ENTER
                | PermissionBits::SPEAK
                | PermissionBits::WHISPER
                | PermissionBits::TEXT_MESSAGE,
        );
        view.permissions = view
            .channels
            .keys()
            .cloned()
            .map(|key| (key, effective))
            .collect();
        view.server_presentation = snapshot.presentation.clone();

        // A receiver hears exactly the members its own view projects, itself
        // excluded: server loopback is the runtime's own path, not a route.
        let audio_routes = snapshot
            .members
            .iter()
            .filter(|(sender, member)| member.realm == viewer.realm && **sender != connection)
            .map(|(sender, _member)| DesiredAudioRoute {
                sender: *sender,
                receiver: connection,
            })
            .collect();

        Ok(RenderOutput::new(
            view,
            audio_routes,
            InteractionRegistry::default(),
        ))
    }

    fn observe(&self, event: &VoiceEvent) {
        match event {
            VoiceEvent::Connected {
                connection,
                name,
                certificate_hash,
                ..
            } => self.admit(*connection, name, certificate_hash.clone()),
            VoiceEvent::Disconnected { connection, .. } => self.forget(*connection),
            VoiceEvent::ChannelInteractionRequested {
                connection,
                channel,
                ..
            } => {
                // Entering a realm channel is this flavor's only mutation. A
                // request naming anything else is simply not something this
                // model does, and refusing it means changing nothing.
                if let Some(realm) = Realm::from_key(channel) {
                    self.move_to_realm(*connection, realm);
                }
            }
            // A connection that never authenticated has no membership to undo.
            // The contract is `#[non_exhaustive]`, so a future event kind also
            // lands here and leaves this model untouched, which is the correct
            // business answer for an event it does not model.
            _ => {}
        }
    }
}

/// Split a raw username into its display name and the realm it selects.
///
/// The `@aurora` / `@borealis` suffix is this flavor's naming convention and
/// nothing in Voxloom knows about it. A name without one starts in Aurora,
/// which keeps the plain Phase 3 scenario working.
fn identity(raw: &str) -> (String, Realm) {
    let lower = raw.to_ascii_lowercase();
    let (display, realm) = if let Some(display) = strip_suffix(raw, &lower, "@borealis") {
        (display, Realm::Borealis)
    } else if let Some(display) = strip_suffix(raw, &lower, "@aurora") {
        (display, Realm::Aurora)
    } else {
        (raw, Realm::Aurora)
    };
    let display = display.trim();
    let display = if display.is_empty() {
        "Guest".to_owned()
    } else {
        display.chars().take(64).collect()
    };
    (display, realm)
}

fn strip_suffix<'a>(raw: &'a str, lower: &str, suffix: &str) -> Option<&'a str> {
    if !lower.ends_with(suffix) {
        return None;
    }
    let prefix_length = raw.len().checked_sub(suffix.len())?;
    raw.get(..prefix_length)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use voxloom_control::{render_snapshot, validate_rendered_snapshot};

    use super::*;

    const ALICE: ConnectionId = ConnectionId::new(1);
    const BOB: ConnectionId = ConnectionId::new(2);

    fn flavor() -> ReferenceFlavor {
        ReferenceFlavor::new("Voxloom", ServerPresentation::default())
    }

    fn connect(flavor: &ReferenceFlavor, connection: ConnectionId, name: &str) {
        flavor.observe(&VoiceEvent::Connected {
            connection,
            generation: 0,
            name: name.to_owned(),
            certificate_hash: Some(format!("{name}-cert")),
        });
    }

    fn output(
        flavor: &ReferenceFlavor,
        snapshot: &Snapshot,
        connection: ConnectionId,
    ) -> RenderOutput {
        match flavor.render(snapshot, connection) {
            Ok(output) => output,
            Err(error) => panic!("render failed: {error}"),
        }
    }

    fn projected_names(output: &RenderOutput) -> Vec<String> {
        output
            .client_view()
            .users
            .values()
            .map(|user| user.name.clone())
            .collect()
    }

    #[test]
    fn a_name_suffix_selects_a_realm_without_changing_the_display_name() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice@borealis");
        connect(&flavor, BOB, "bob");

        let snapshot = flavor.snapshot();
        assert_eq!(snapshot.realm_of(ALICE), Some(Realm::Borealis));
        assert_eq!(snapshot.realm_of(BOB), Some(Realm::Aurora));
        assert_eq!(
            projected_names(&output(&flavor, &snapshot, ALICE)),
            vec!["alice".to_owned()]
        );
    }

    #[test]
    fn members_of_different_realms_hold_different_trees_and_no_route() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice@aurora");
        connect(&flavor, BOB, "bob@borealis");
        let snapshot = flavor.snapshot();

        let alice = output(&flavor, &snapshot, ALICE);
        let bob = output(&flavor, &snapshot, BOB);

        assert_ne!(alice.client_view(), bob.client_view());
        assert_eq!(projected_names(&alice), vec!["alice".to_owned()]);
        assert_eq!(projected_names(&bob), vec!["bob".to_owned()]);
        assert!(alice.audio_routes().is_empty());
        assert!(bob.audio_routes().is_empty());
    }

    #[test]
    fn members_of_one_realm_get_directional_routes_owned_by_the_receiver() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice");
        connect(&flavor, BOB, "bob");
        let snapshot = flavor.snapshot();

        assert_eq!(
            output(&flavor, &snapshot, ALICE).audio_routes(),
            &BTreeSet::from([DesiredAudioRoute {
                sender: BOB,
                receiver: ALICE,
            }])
        );
        assert_eq!(
            output(&flavor, &snapshot, BOB).audio_routes(),
            &BTreeSet::from([DesiredAudioRoute {
                sender: ALICE,
                receiver: BOB,
            }])
        );
    }

    #[test]
    fn a_channel_interaction_moves_a_member_and_advances_the_revision() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice");
        connect(&flavor, BOB, "bob");
        let before = flavor.snapshot();

        flavor.observe(&VoiceEvent::ChannelInteractionRequested {
            connection: ALICE,
            generation: 3,
            channel: Realm::Borealis.key(),
        });
        let after = flavor.snapshot();

        assert_ne!(after.revision(), before.revision());
        assert_eq!(after.realm_of(ALICE), Some(Realm::Borealis));
        // The snapshot Voxloom may still be rendering did not move under it.
        assert_eq!(before.realm_of(ALICE), Some(Realm::Aurora));
        assert!(output(&flavor, &after, BOB).audio_routes().is_empty());
    }

    #[test]
    fn an_interaction_this_model_does_not_know_changes_nothing() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice");
        let before = flavor.snapshot();

        flavor.observe(&VoiceEvent::ChannelInteractionRequested {
            connection: ALICE,
            generation: 1,
            channel: ChannelKey(SemanticKey::Static("root".to_owned())),
        });

        assert_eq!(flavor.snapshot().revision(), before.revision());
    }

    #[test]
    fn a_departure_removes_the_member_and_its_routes() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice");
        connect(&flavor, BOB, "bob");

        flavor.observe(&VoiceEvent::Disconnected {
            connection: BOB,
            generation: 1,
            reason: "transport closed".to_owned(),
        });
        let snapshot = flavor.snapshot();

        assert_eq!(snapshot.realm_of(BOB), None);
        assert!(output(&flavor, &snapshot, ALICE).audio_routes().is_empty());
        match flavor.render(&snapshot, BOB) {
            Err(error) => assert_eq!(error.connection(), BOB),
            Ok(_output) => panic!("a departed member must not render"),
        }
    }

    #[test]
    fn every_rendered_generation_passes_the_runtime_validation() {
        let flavor = flavor();
        connect(&flavor, ALICE, "alice@aurora");
        connect(&flavor, BOB, "bob@borealis");

        for snapshot in [flavor.snapshot(), {
            flavor.observe(&VoiceEvent::ChannelInteractionRequested {
                connection: BOB,
                generation: 1,
                channel: Realm::Aurora.key(),
            });
            flavor.snapshot()
        }] {
            let rendered = match render_snapshot(&flavor, Arc::clone(&snapshot), [ALICE, BOB]) {
                Ok(rendered) => rendered,
                Err(error) => panic!("render failed: {error}"),
            };
            if let Err(error) = validate_rendered_snapshot(rendered) {
                panic!("the reference flavor produced an invalid generation: {error}");
            }
        }
    }
}
