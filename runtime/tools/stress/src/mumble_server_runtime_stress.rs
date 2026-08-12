#![forbid(unsafe_code)]

pub mod audio;
pub mod client;
pub mod config;
pub mod scenario;
pub mod stats;

pub use audio::VoiceClip;
pub use client::{
    ClientAction, ClientControlError, ClientEvent, ManagedClient, ManagedClientConfig,
    MumbleCredential, spawn_managed,
};
pub use config::{Config, ScenarioKind};
pub use stats::{ClientReport, Stats, StatsSummary};
