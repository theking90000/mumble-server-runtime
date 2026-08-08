//! One executable: the gateway, both shards, and the demo flavor.
//!
//! This is the only file in the repository that names both the runtime and a
//! concrete business. Everything above it is generic; everything below it is a
//! game. That split is the point of the crate boundary, not an accident of
//! layout.
//!
//! ```text
//!   cargo run -p mumble-server-runtime-arena
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
use std::time::Duration;

use anyhow::{Context, Result};
use mumble_server_runtime_arena::arena::Arena;
use mumble_server_runtime_arena::directory::{Destinations, Directory};
use mumble_server_runtime_arena::lobby::{Choices, Lobby, LobbyUpdate};
use mumble_server_runtime_arena::router::ArenaRouter;
use mumble_server_runtime_gateway::tls::Identity;
use mumble_server_runtime_gateway::{Gateway, GatewayConfig};

async fn publish_lobby_updates(
    sender: tokio::sync::mpsc::Sender<LobbyUpdate>,
    handle: mumble_server_runtime_shard::ShardHandle,
) -> Result<()> {
    let period = Duration::from_secs(1);
    let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut counter = 0_u64;

    loop {
        ticks.tick().await;
        counter = counter.saturating_add(1);
        sender
            .send(LobbyUpdate::Counter(counter))
            .await
            .context("sending the lobby counter update")?;
        handle.wake();
    }
}

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
        welcome_text: "Mumble Server Runtime arena. Double-click a channel to choose.".to_owned(),
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
    let (lobby_updates, lobby_inbox) = tokio::sync::mpsc::channel(1);
    let lobby_directory = Arc::clone(&directory);
    let lobby_destinations = Arc::clone(&destinations);
    let lobby_chosen = Arc::clone(&chosen);
    let lobby = runtime.create_shard(move |_handle| {
        Lobby::new(
            lobby_directory,
            lobby_destinations,
            lobby_chosen,
            lobby_inbox,
        )
    });
    let arena = runtime.create_shard(|_handle| {
        Arena::new(
            Arc::clone(&directory),
            Arc::clone(&destinations),
            Arc::clone(&chosen),
        )
    });
    destinations.set_lobby(lobby.shard());
    destinations.set_arena(arena.shard());

    println!(
        "mumble-server-runtime-arena listening on {}",
        gateway.address()
    );
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

    let mut lobby_updates_task = tokio::spawn(publish_lobby_updates(lobby_updates, lobby.clone()));
    tokio::select! {
        // Cancellation-safe for shutdown: this branch owns the gateway, and
        // dropping the other future does not detach the update task because it
        // is explicitly aborted and joined below.
        result = gateway.serve(ArenaRouter::new(directory, destinations)) => {
            lobby_updates_task.abort();
            match lobby_updates_task.await {
                Err(error) if error.is_cancelled() => result.context("serving"),
                Ok(Ok(())) => result.context("serving"),
                Ok(Err(error)) => Err(error.context("publishing lobby updates")),
                Err(error) => Err(error).context("joining the lobby update task"),
            }
        }
        // Cancellation-safe: awaiting a JoinHandle by mutable reference leaves
        // the task and its output available when the other branch wins.
        result = &mut lobby_updates_task => match result {
            Ok(Ok(())) => anyhow::bail!("the lobby update task stopped unexpectedly"),
            Ok(Err(error)) => Err(error.context("publishing lobby updates")),
            Err(error) => Err(error).context("joining the lobby update task"),
        },
    }
}
