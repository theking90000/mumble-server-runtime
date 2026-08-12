use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use mumble_server_runtime_stress::VoiceClip;
use mumble_server_runtime_stress::audio;
use mumble_server_runtime_stress::client;
use mumble_server_runtime_stress::config::Config;
use mumble_server_runtime_stress::stats::{ClientReport, Stats};
use tokio::task::JoinSet;
use tokio::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let config = Arc::new(Config::parse());
    config.validate()?;
    let voice_clip = config
        .voice_file
        .as_deref()
        .map(|path| VoiceClip::load(path, config.voice_frame_bytes))
        .transpose()?
        .map(Arc::new);
    let _installed = rustls::crypto::ring::default_provider().install_default();
    let connector = client::tls_connector();

    let started = Instant::now();
    let clients = config.clients.get();
    let workload_starts = started + config.ramp;
    let stop_at = workload_starts + config.duration;
    let mut tasks = JoinSet::new();

    println!(
        "starting {clients} {:?} client(s) against {} over {:?}, then holding for {:?}",
        config.scenario, config.server, config.ramp, config.duration
    );
    if let Some(clip) = &voice_clip {
        println!(
            "Opus UDP voice enabled: {} frames of {} bytes every {:?}, {}% talk time in {:?} spurts per client",
            clip.frames(),
            clip.frame_bytes(),
            audio::OPUS_FRAME_DURATION,
            config.talk_percent,
            config.talk_spurt
        );
    }

    for client_number in 0..clients {
        let fraction = if clients == 1 {
            0.0
        } else {
            client_number as f64 / clients.saturating_sub(1) as f64
        };
        let launch_at = started + config.ramp.mul_f64(fraction);
        let config = Arc::clone(&config);
        let voice_clip = voice_clip.as_ref().map(Arc::clone);
        let connector = connector.clone();
        let _task = tasks.spawn(async move {
            client::run(
                config,
                voice_clip,
                connector,
                client_number,
                launch_at,
                stop_at,
            )
            .await
        });
    }

    let mut stats = Stats::default();
    let mut progress = tokio::time::interval(std::time::Duration::from_secs(1));
    progress.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !tasks.is_empty() {
        tokio::select! {
            // Cancellation-safe: JoinSet keeps completed outputs for the next poll.
            joined = tasks.join_next() => {
                if let Some(joined) = joined {
                    match joined {
                        Ok(report) => stats.record(report),
                        Err(error) => stats.record(ClientReport {
                            error: Some(format!("client task failed: {error}")),
                            ..ClientReport::default()
                        }),
                    }
                }
            }
            // Cancellation-safe: missed progress ticks are deliberately skipped.
            _ = progress.tick() => {
                println!(
                    "progress: {}/{} finished, {} successful",
                    stats.reports(),
                    clients,
                    stats.completed()
                );
            }
        }
    }

    let elapsed = started.elapsed();
    stats.print(elapsed);
    if let Some(path) = &config.json_output {
        let summary = stats.summary(elapsed);
        let file = std::fs::File::create(path)
            .with_context(|| format!("creating JSON report {}", path.display()))?;
        serde_json::to_writer_pretty(file, &summary)
            .with_context(|| format!("writing JSON report {}", path.display()))?;
    }
    ensure!(
        stats.failure_rate() <= config.failure_threshold,
        "failure rate {:.2}% exceeds threshold {:.2}%",
        stats.failure_rate() * 100.0,
        config.failure_threshold * 100.0
    );
    Ok(())
}
