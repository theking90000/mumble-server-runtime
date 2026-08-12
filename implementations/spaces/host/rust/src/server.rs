use std::net::SocketAddr;
#[cfg(feature = "load-metrics")]
use std::sync::Arc;

use mumble_server_runtime_gateway::tls::Identity;
use mumble_server_runtime_gateway::{Gateway, GatewayConfig};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;

use crate::actor;
use crate::config::{ConfigError, ControllerConfig};
use crate::core_protocol::controller_service_server::ControllerServiceServer as CoreServiceServer;
#[cfg(feature = "load-metrics")]
use crate::metrics::{ActorMetrics, MetricsOutput, spawn_writer};
use crate::service::{ControllerRouter, service};

/// Both public listeners and their owned background tasks.
pub struct RunningControllerServer {
    controller_address: SocketAddr,
    mumble_address: SocketAddr,
    grpc_shutdown: Option<oneshot::Sender<()>>,
    grpc_task: Option<JoinHandle<Result<(), tonic::transport::Error>>>,
    gateway_task: Option<JoinHandle<Result<(), String>>>,
    actor_task: Option<JoinHandle<()>>,
    #[cfg(feature = "load-metrics")]
    metrics_task: Option<JoinHandle<Result<(), std::io::Error>>>,
}

impl RunningControllerServer {
    /// Bind gRPC and Mumble, then start the Controller actor and both serving planes.
    pub async fn start(
        config: ControllerConfig,
        identity: Identity,
    ) -> Result<Self, ServerStartError> {
        #[cfg(feature = "load-metrics")]
        {
            Self::start_inner(config, identity, None).await
        }
        #[cfg(not(feature = "load-metrics"))]
        {
            Self::start_inner(config, identity).await
        }
    }

    /// Start the server with an optional periodic, secret-free JSONL metrics writer.
    #[cfg(feature = "load-metrics")]
    pub async fn start_with_metrics(
        config: ControllerConfig,
        identity: Identity,
        metrics_output: Option<MetricsOutput>,
    ) -> Result<Self, ServerStartError> {
        Self::start_inner(config, identity, metrics_output).await
    }

    async fn start_inner(
        config: ControllerConfig,
        identity: Identity,
        #[cfg(feature = "load-metrics")] metrics_output: Option<MetricsOutput>,
    ) -> Result<Self, ServerStartError> {
        config.validate()?;
        #[cfg(feature = "load-metrics")]
        if metrics_output
            .as_ref()
            .is_some_and(|output| output.interval.is_zero())
        {
            return Err(ServerStartError::ZeroMetricsInterval);
        }
        let gateway = Gateway::bind(
            GatewayConfig {
                bind: config.mumble_bind,
                max_users: config.max_mumble_connections,
                ..GatewayConfig::default()
            },
            identity,
        )
        .await
        .map_err(|error| ServerStartError::MumbleBind(error.to_string()))?;
        let mumble_address = gateway.address();
        let runtime = gateway.runtime();
        #[cfg(feature = "load-metrics")]
        let voice_metrics = gateway.voice_metrics();

        let listener = TcpListener::bind(config.controller_bind)
            .await
            .map_err(ServerStartError::ControllerBind)?;
        let controller_address = listener
            .local_addr()
            .map_err(ServerStartError::ControllerAddress)?;

        #[cfg(feature = "load-metrics")]
        let actor_metrics = Arc::new(ActorMetrics::default());
        #[cfg(feature = "load-metrics")]
        let (actor, actor_task) =
            actor::spawn(config.clone(), runtime.clone(), Arc::clone(&actor_metrics))?;
        #[cfg(not(feature = "load-metrics"))]
        let (actor, actor_task) = actor::spawn(config.clone(), runtime.clone())?;
        let router = ControllerRouter::new(actor.clone());
        let gateway_task = tokio::spawn(async move {
            gateway
                .serve(router)
                .await
                .map_err(|error| error.to_string())
        });

        let controller_service = service(actor, config.queue_capacity);
        let core_grpc = CoreServiceServer::from_arc(controller_service)
            .max_decoding_message_size(config.grpc_max_frame_bytes)
            .max_encoding_message_size(config.grpc_max_frame_bytes);
        let (grpc_shutdown, shutdown) = oneshot::channel();
        let grpc_task = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(core_grpc)
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _closed = shutdown.await;
                }),
        );
        #[cfg(feature = "load-metrics")]
        let metrics_task = metrics_output
            .map(|output| spawn_writer(output, actor_metrics, voice_metrics, runtime));

        Ok(Self {
            controller_address,
            mumble_address,
            grpc_shutdown: Some(grpc_shutdown),
            grpc_task: Some(grpc_task),
            gateway_task: Some(gateway_task),
            actor_task: Some(actor_task),
            #[cfg(feature = "load-metrics")]
            metrics_task,
        })
    }

    #[must_use]
    pub fn controller_address(&self) -> SocketAddr {
        self.controller_address
    }

    #[must_use]
    pub fn mumble_address(&self) -> SocketAddr {
        self.mumble_address
    }

    /// Stop all owned tasks. Repeated process-level shutdown is harmless.
    pub async fn shutdown(mut self) {
        if let Some(shutdown) = self.grpc_shutdown.take() {
            let _receiver_gone = shutdown.send(());
        }
        if let Some(task) = self.grpc_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("mumble-spaces-server: gRPC server stopped: {error}")
                }
                Err(error) => eprintln!("mumble-spaces-server: gRPC task failed: {error}"),
            }
        }
        if let Some(task) = &self.gateway_task {
            task.abort();
        }
        if let Some(task) = &self.actor_task {
            task.abort();
        }
        #[cfg(feature = "load-metrics")]
        if let Some(task) = &self.metrics_task {
            task.abort();
        }
        if let Some(task) = self.gateway_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("mumble-spaces-server: Mumble gateway stopped: {error}")
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => eprintln!("mumble-spaces-server: Mumble task failed: {error}"),
            }
        }
        if let Some(task) = self.actor_task.take() {
            match task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => eprintln!("mumble-spaces-server: actor task failed: {error}"),
            }
        }
        #[cfg(feature = "load-metrics")]
        if let Some(task) = self.metrics_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("mumble-spaces-server: metrics writer stopped: {error}")
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => eprintln!("mumble-spaces-server: metrics task failed: {error}"),
            }
        }
    }
}

impl Drop for RunningControllerServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.grpc_shutdown.take() {
            let _receiver_gone = shutdown.send(());
        }
        if let Some(task) = &self.grpc_task {
            task.abort();
        }
        if let Some(task) = &self.gateway_task {
            task.abort();
        }
        if let Some(task) = &self.actor_task {
            task.abort();
        }
        #[cfg(feature = "load-metrics")]
        if let Some(task) = &self.metrics_task {
            task.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerStartError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("binding the Mumble listener failed: {0}")]
    MumbleBind(String),
    #[error("binding the Controller listener failed: {0}")]
    ControllerBind(#[source] std::io::Error),
    #[error("reading the bound Controller address failed: {0}")]
    ControllerAddress(#[source] std::io::Error),
    #[error(transparent)]
    Actor(#[from] actor::ActorStartError),
    #[cfg(feature = "load-metrics")]
    #[error("metrics interval must be positive")]
    ZeroMetricsInterval,
}

#[cfg(all(test, feature = "load-metrics"))]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[tokio::test]
    async fn zero_metrics_interval_is_rejected_before_binding()
    -> Result<(), Box<dyn std::error::Error>> {
        let identity = Identity::self_signed(vec!["localhost".to_owned()])?;
        let result = RunningControllerServer::start_with_metrics(
            ControllerConfig::default(),
            identity,
            Some(MetricsOutput {
                path: PathBuf::from("unused.jsonl"),
                interval: Duration::ZERO,
            }),
        )
        .await;

        assert!(matches!(result, Err(ServerStartError::ZeroMetricsInterval)));
        Ok(())
    }
}
