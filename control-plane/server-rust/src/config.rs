use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use thiserror::Error;

/// Resource and network limits for one Controller process.
#[derive(Debug, Clone)]
pub struct ControllerConfig {
    pub controller_bind: SocketAddr,
    pub mumble_bind: SocketAddr,
    pub lease_duration: Duration,
    pub empty_space_grace: Duration,
    pub max_sessions: usize,
    pub max_participants: usize,
    pub max_participants_per_session: usize,
    pub max_spaces: usize,
    pub max_observations_per_session: usize,
    pub queue_capacity: usize,
    pub grpc_max_frame_bytes: usize,
    pub max_mumble_connections: u32,
    pub allow_unauthenticated_controller_network: bool,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            controller_bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 4_000)),
            mumble_bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 64_738)),
            lease_duration: Duration::from_secs(30),
            empty_space_grace: Duration::from_secs(30),
            max_sessions: 64,
            max_participants: 10_000,
            max_participants_per_session: 5_000,
            max_spaces: 1_024,
            max_observations_per_session: 1_024,
            queue_capacity: 1_024,
            grpc_max_frame_bytes: 4 * 1_024 * 1_024,
            max_mumble_connections: 100,
            allow_unauthenticated_controller_network: false,
        }
    }
}

impl ControllerConfig {
    /// Reject unsafe or unusable process configuration before binding sockets.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !is_loopback(self.controller_bind.ip()) && !self.allow_unauthenticated_controller_network
        {
            return Err(ConfigError::UnauthenticatedNetworkBind(
                self.controller_bind,
            ));
        }
        if self.lease_duration.is_zero() {
            return Err(ConfigError::ZeroLease);
        }
        if self.queue_capacity == 0 {
            return Err(ConfigError::ZeroQueue);
        }
        if self.max_sessions == 0
            || self.max_participants == 0
            || self.max_participants_per_session == 0
            || self.max_spaces == 0
            || self.max_observations_per_session == 0
            || self.grpc_max_frame_bytes == 0
            || self.max_mumble_connections == 0
        {
            return Err(ConfigError::ZeroLimit);
        }
        Ok(())
    }
}

fn is_loopback(address: IpAddr) -> bool {
    address.is_loopback()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error(
        "controller bind {0} is not loopback; pass --allow-unauthenticated-controller-network to acknowledge plaintext unauthenticated access"
    )]
    UnauthenticatedNetworkBind(SocketAddr),
    #[error("controller lease duration must be positive")]
    ZeroLease,
    #[error("controller queue capacity must be positive")]
    ZeroQueue,
    #[error("controller resource limits must be positive")]
    ZeroLimit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_controller_bind_requires_explicit_acknowledgement() {
        let mut config = ControllerConfig {
            controller_bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, 4_000)),
            ..ControllerConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnauthenticatedNetworkBind(_))
        ));

        config.allow_unauthenticated_controller_network = true;
        assert_eq!(config.validate(), Ok(()));
    }
}
