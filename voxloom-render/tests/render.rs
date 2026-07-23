//! Static-invariant and normalization tests for `voxloom-render`.
//!
//! Each section-20 invariant enforced by [`validate`] has a test that fails if
//! its check is removed from the validator (spec 26.6 mutation rule): the test
//! builds a view violating exactly that invariant and asserts the named
//! `Invariant` is returned. Deleting the corresponding check makes `validate`
//! return `Ok` (or a different invariant), and the test fails.

// Tests build hostile views by hand and assert on results; unwrap/expect are the
// clearest way to state "this must hold or the test is broken".
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};

use voxloom_render::{
    ChannelId, ChannelKey, ClientView, Invariant, ListenerRelation, PermissionBits, SemanticKey,
    SessionId, ViewChannel, ViewUser, normalize, validate,
};

fn channel(id: u32, parent: u32, name: &str) -> ViewChannel {
    ViewChannel {
        key: ChannelKey(SemanticKey::Static(format!("ch{id}"))),
        id: ChannelId(id),
        parent: ChannelId(parent),
        name: name.to_owned(),
        description: None,
        position: 0,
        temporary: false,
        max_users: None,
        enter_restricted: false,
        can_enter: true,
        links: BTreeSet::new(),
    }
}

fn user(session: u32, channel: u32, name: &str) -> ViewUser {
    ViewUser {
        key: UserKeyStatic(session),
        session: SessionId(session),
        name: name.to_owned(),
        channel: ChannelId(channel),
        user_id: None,
        certificate_hash: None,
        mute: false,
        deaf: false,
        suppress: false,
        self_mute: false,
        self_deaf: false,
        priority_speaker: false,
        recording: false,
        comment: None,
        texture: None,
    }
}

#[allow(non_snake_case)] // reads as a small constructor at call sites
fn UserKeyStatic(session: u32) -> voxloom_render::UserKey {
    voxloom_render::UserKey(SemanticKey::Static(format!("u{session}")))
}

/// A minimal valid view: root, one child, and a self-user in the root.
fn base_view() -> (ClientView, SessionId) {
    let mut channels = BTreeMap::new();
    channels.insert(ChannelId(0), channel(0, 0, "root"));
    channels.insert(ChannelId(1), channel(1, 0, "child"));
    let mut users = BTreeMap::new();
    users.insert(SessionId(1), user(1, 0, "self"));
    let view = ClientView {
        root_channel: ChannelId(0),
        channels,
        users,
        listeners: BTreeSet::new(),
        permissions: BTreeMap::new(),
        context_actions: BTreeMap::new(),
        server_presentation: Default::default(),
    };
    (view, SessionId(1))
}

#[test]
fn base_view_is_valid() {
    let (view, self_session) = base_view();
    validate(&view, Some(self_session)).expect("base view should be valid");
}

#[test]
fn invariant_1_root_must_exist() {
    let (mut view, self_session) = base_view();
    view.root_channel = ChannelId(99); // points at a channel that is not present
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::RootChannelExists);
}

#[test]
fn invariant_3_child_needs_visible_parent() {
    let (mut view, self_session) = base_view();
    view.channels.insert(ChannelId(2), channel(2, 42, "orphan")); // parent 42 absent
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::ChildHasVisibleParent);
}

#[test]
fn invariant_4_no_parent_cycle() {
    let (mut view, self_session) = base_view();
    // 2 -> 3 -> 2, both visible, neither the root: a pure cycle.
    view.channels.insert(ChannelId(2), channel(2, 3, "a"));
    view.channels.insert(ChannelId(3), channel(3, 2, "b"));
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::NoParentCycle);
}

#[test]
fn invariant_5_user_in_visible_channel() {
    let (mut view, self_session) = base_view();
    view.users.insert(SessionId(2), user(2, 77, "ghost")); // channel 77 absent
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::UserInVisibleChannel);
}

#[test]
fn invariant_6_self_user_present() {
    let (mut view, self_session) = base_view();
    view.users.remove(&self_session);
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::SelfUserPresent);
}

#[test]
fn invariant_6_skipped_before_sync() {
    // The pre-ServerSync view legitimately has no self-user yet.
    let (mut view, _self) = base_view();
    view.users.clear();
    validate(&view, None).expect("no self required when None is passed");
}

#[test]
fn invariant_11_channel_id_matches_key() {
    let (mut view, self_session) = base_view();
    // Keyed by 1 but declares id 2: a duplicate/inconsistent id.
    view.channels
        .insert(ChannelId(1), channel(2, 0, "mismatch"));
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::UniqueIds);
}

#[test]
fn invariant_15_listener_must_reference_visible_entities() {
    let (mut view, self_session) = base_view();
    view.listeners.insert(ListenerRelation {
        user: SessionId(1),
        channel: ChannelId(555), // channel not visible
    });
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::NoInvisibleActorReferenced);
}

#[test]
fn invariant_15_link_must_reference_visible_channel() {
    let (mut view, self_session) = base_view();
    let mut linked = channel(2, 0, "linker");
    linked.links.insert(ChannelId(999)); // link target not visible
    view.channels.insert(ChannelId(2), linked);
    let err = validate(&view, Some(self_session)).unwrap_err();
    assert_eq!(err.invariant, Invariant::NoInvisibleActorReferenced);
}

#[test]
fn normalize_is_idempotent() {
    let (view, _self) = base_view();
    let once = normalize(&view);
    let twice = normalize(&once);
    assert_eq!(once, twice);
}

#[test]
fn normalize_drops_empty_permission_entries() {
    let (mut view, _self) = base_view();
    view.permissions.insert(ChannelId(1), PermissionBits::NONE);
    let normalized = normalize(&view);
    assert!(
        !normalized.permissions.contains_key(&ChannelId(1)),
        "an all-zero permission mask carries no information and is dropped"
    );
}

#[test]
fn normalize_drops_self_links() {
    let (mut view, _self) = base_view();
    let mut selfish = channel(2, 0, "selflink");
    selfish.links.insert(ChannelId(2)); // links to itself
    view.channels.insert(ChannelId(2), selfish);
    let normalized = normalize(&view);
    assert!(
        normalized.channels[&ChannelId(2)].links.is_empty(),
        "a self-link is inert and is removed"
    );
}
