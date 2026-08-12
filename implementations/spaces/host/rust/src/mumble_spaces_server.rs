//! Runnable Rust host for the provided Spaces implementation.
#![forbid(unsafe_code)]

mod actor;
mod actor_messages;
pub mod config;
mod metrics;
mod profile;
mod server;
mod service;
mod wire;

pub mod core_protocol {
    tonic::include_proto!("mumble.controller.core.v1");
}

pub use config::ControllerConfig;
pub use metrics::MetricsOutput;
pub use server::{RunningControllerServer, ServerStartError};
