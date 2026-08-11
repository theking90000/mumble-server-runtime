//! Rust application implementing the Mumble Controller contract.
#![forbid(unsafe_code)]

mod actor;
mod actor_messages;
pub mod config;
mod profile;
mod server;
mod service;
mod wire;

pub mod core_protocol {
    tonic::include_proto!("mumble.controller.core.v1");
}

pub use config::ControllerConfig;
pub use server::{RunningControllerServer, ServerStartError};
