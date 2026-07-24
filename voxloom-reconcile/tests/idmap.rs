//! Tests for per-connection key-to-id stability (ADR-007, spec 9.2/9.3, and
//! invariant 12: released ids are not reused immediately).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use voxloom_reconcile::{ChannelIdKind, ViewIdMapping};
use voxloom_render::{ChannelKey, SemanticKey};

fn key(name: &str) -> ChannelKey {
    ChannelKey(SemanticKey::Static(name.to_owned()))
}

const EPHEMERAL_BASE: u32 = 1 << 30;

#[test]
fn same_key_resolves_to_same_id() {
    let mut mapping = ViewIdMapping::new();
    let first = mapping
        .resolve(key("lobby"), ChannelIdKind::Stable)
        .unwrap();
    let second = mapping
        .resolve(key("lobby"), ChannelIdKind::Stable)
        .unwrap();
    assert_eq!(first, second, "a key keeps its id across renders (ADR-007)");
}

#[test]
fn distinct_keys_get_distinct_ids() {
    let mut mapping = ViewIdMapping::new();
    let a = mapping.resolve(key("a"), ChannelIdKind::Stable).unwrap();
    let b = mapping.resolve(key("b"), ChannelIdKind::Stable).unwrap();
    assert_ne!(a, b);
}

#[test]
fn stable_and_ephemeral_ids_live_in_separate_ranges() {
    let mut mapping = ViewIdMapping::new();
    let stable = mapping
        .resolve(key("stable"), ChannelIdKind::Stable)
        .unwrap();
    let ephemeral = mapping
        .resolve(key("ephemeral"), ChannelIdKind::Ephemeral)
        .unwrap();
    assert!(
        stable.0 > 0 && stable.0 < EPHEMERAL_BASE,
        "stable id below base"
    );
    assert!(
        ephemeral.0 >= EPHEMERAL_BASE,
        "ephemeral id at or above base"
    );
}

#[test]
fn root_id_zero_is_never_allocated() {
    let mut mapping = ViewIdMapping::new();
    for index in 0..1000u32 {
        let id = mapping
            .resolve(key(&format!("c{index}")), ChannelIdKind::Stable)
            .unwrap();
        assert_ne!(id.0, 0, "id 0 is reserved for the root");
    }
}

#[test]
fn released_id_is_not_reused_for_a_new_key() {
    let mut mapping = ViewIdMapping::new();
    let released = mapping.resolve(key("temp"), ChannelIdKind::Stable).unwrap();
    assert_eq!(mapping.release(&key("temp")), Some(released));
    // Re-adding the same key, and adding a brand-new key, must both avoid the
    // just-released id (invariant 12: no immediate reuse).
    let readded = mapping.resolve(key("temp"), ChannelIdKind::Stable).unwrap();
    let fresh = mapping
        .resolve(key("other"), ChannelIdKind::Stable)
        .unwrap();
    assert_ne!(readded, released);
    assert_ne!(fresh, released);
    assert_ne!(readded, fresh);
}

#[test]
fn reverse_lookup_resolves_client_id_to_key() {
    let mut mapping = ViewIdMapping::new();
    let id = mapping
        .resolve(key("party"), ChannelIdKind::Ephemeral)
        .unwrap();
    assert_eq!(mapping.key_of(id), Some(&key("party")));
    assert_eq!(mapping.get(&key("party")), Some(id));
}
