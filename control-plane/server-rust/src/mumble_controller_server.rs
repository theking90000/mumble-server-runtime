//! Rust application implementing the Mumble Controller contract.
#![forbid(unsafe_code)]

mod actor;
pub mod config;
mod profile;
mod server;
mod service;

pub mod protocol {
    tonic::include_proto!("mumble.controller.v1");
}

pub use config::ControllerConfig;
pub use server::{RunningControllerServer, ServerStartError};
