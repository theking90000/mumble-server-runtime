//! View normalization (spec 12.3).
//!
//! Normalization produces a deterministic canonical form of a view so that the
//! same canonical state always renders to the same bytes and the diff compares
//! like against like. This crate's `ClientView` already stores its collections
//! in ordered maps and sets, so most of 12.3 (sorting, deterministic layout) is
//! structural. What [`normalize`] still enforces:
//!
//! - collections are keyed by each element's own identity (a channel stored
//!   under a mismatched id is re-keyed to `channel.id`, a user to `user.session`);
//! - inert default values are dropped: a permission entry equal to
//!   [`PermissionBits::NONE`] carries no information and is removed, and a
//!   channel link to the channel itself is removed.
//!
//! Key-to-id resolution (spec 12.3 "resolve keys to ids") happens before this
//! crate sees a view: the reconciler assigns ids from semantic keys with
//! stability guarantees (ADR-007). Reference, cycle, root and self-user checks
//! are [`crate::validate`], run right after normalization (spec 12.2 pipeline).
//!
//! [`normalize`] is idempotent: `normalize(normalize(v)) == normalize(v)`.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::{ChannelId, SessionId};
use crate::view::{ClientView, PermissionBits, ViewChannel, ViewUser};

/// Return the canonical, deterministic form of `view` (spec 12.3).
#[must_use]
pub fn normalize(view: &ClientView) -> ClientView {
    let channels: BTreeMap<ChannelId, ViewChannel> = view
        .channels
        .values()
        .map(|channel| {
            let mut links: BTreeSet<ChannelId> = channel.links.clone();
            // A channel linked to itself is inert; drop it so two views that
            // differ only by a self-link normalize equal.
            links.remove(&channel.id);
            let normalized = ViewChannel {
                links,
                ..channel.clone()
            };
            (channel.id, normalized)
        })
        .collect();

    let users: BTreeMap<SessionId, ViewUser> = view
        .users
        .values()
        .map(|user| (user.session, user.clone()))
        .collect();

    // Drop permission entries that grant nothing: the absence of an entry and an
    // empty mask are the same fact, so keeping the empty entry would make two
    // equal views diff.
    let permissions: BTreeMap<ChannelId, PermissionBits> = view
        .permissions
        .iter()
        .filter(|(_, bits)| **bits != PermissionBits::NONE)
        .map(|(id, bits)| (*id, *bits))
        .collect();

    ClientView {
        root_channel: view.root_channel,
        channels,
        users,
        listeners: view.listeners.clone(),
        permissions,
        context_actions: view.context_actions.clone(),
        server_presentation: view.server_presentation.clone(),
    }
}
