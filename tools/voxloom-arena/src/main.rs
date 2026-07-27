//! One executable: the gateway, both shards, and the demo flavor.
//!
//! This is the only file in the repository that names both the runtime and a
//! concrete business. Everything above it is generic; everything below it is a
//! game. That split is the point of the crate boundary, not an accident of
//! layout.
//!
//! ```text
//!   cargo run -p voxloom-arena
//!   then connect a Mumble client to 127.0.0.1:64738
//!   (127.0.0.1, not localhost: that resolves to IPv6 first, which is not bound)
//! ```
//!
//! Two clients, and the whole model is visible by hand: pick different teams in
//! the lobby, enter the arena, and neither can see the other's base. Connect a
//! third with `overwatch` in the password field and it disappears from every
//! view but its own, hears both teams, and becomes visible to exactly one team
//! the moment it double-clicks that team's base.

use std::sync::Arc;

use anyhow::{Context, Result};
use voxloom_arena::arena::Arena;
use voxloom_arena::directory::{Destinations, Directory};
use voxloom_arena::lobby::{Choices, Lobby};
use voxloom_arena::router::ArenaRouter;
use voxloom_gateway::tls::Identity;
use voxloom_gateway::{Gateway, GatewayConfig};

#[tokio::main]
async fn main() -> Result<()> {
    let default = GatewayConfig::default();
    // One optional argument: where to listen. A machine already running a Mumble
    // server holds 64738, and failing to start over that would be a poor
    // introduction to a demo.
    let bind = match std::env::args().nth(1) {
        Some(argument) => argument
            .parse()
            .with_context(|| format!("{argument} is not an address like 0.0.0.0:64738"))?,
        None => default.bind,
    };
    // A second optional argument raises the admission ceiling for load tests
    // while preserving the demo's conservative default.
    let max_users = match std::env::args().nth(2) {
        Some(argument) => argument
            .parse()
            .with_context(|| format!("{argument} is not a maximum user count"))?,
        None => default.max_users,
    };
    let config = GatewayConfig {
        bind,
        max_users,
        welcome_text: "Voxloom arena. Double-click a channel to choose.".to_owned(),
        ..default
    };

    // A fresh certificate every start. Real deployments keep one, so the
    // client's per-server certificate hash stays stable and it does not ask
    // about a new server each time.
    let identity = Identity::self_signed(vec!["localhost".to_owned()])
        .context("generating the server certificate")?;

    let gateway = Gateway::bind(config, identity)
        .await
        .context("binding the gateway")?;
    let runtime = gateway.runtime();

    let directory = Arc::new(Directory::new());
    let destinations = Arc::new(Destinations::new());
    let chosen = Arc::new(Choices::new());

    // Both shards exist before the first connection can be accepted, which is
    // what makes the router's "attach to the lobby" answer always true.
    let lobby = runtime.create_shard(|_handle| {
        Lobby::new(
            Arc::clone(&directory),
            Arc::clone(&destinations),
            runtime.clone(),
            Arc::clone(&chosen),
        )
    });
    let arena = runtime.create_shard(|_handle| {
        Arena::new(
            Arc::clone(&directory),
            Arc::clone(&destinations),
            runtime.clone(),
            Arc::clone(&chosen),
        )
    });
    destinations.set_lobby(lobby.shard());
    destinations.set_arena(arena.shard());

    println!("voxloom-arena listening on {}", gateway.address());
    println!(
        "  lobby shard {:?}, arena shard {:?}",
        lobby.shard(),
        arena.shard()
    );
    println!(
        "  connect a Mumble client to 127.0.0.1:{}",
        gateway.address().port()
    );
    println!("  admission ceiling: {max_users} clients");
    println!("  password 'overwatch' joins as vanished staff");

    gateway
        .serve(ArenaRouter::new(directory, destinations))
        .await
        .context("serving")
}
