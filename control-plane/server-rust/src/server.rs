use std::net::SocketAddr;

use mumble_server_runtime_gateway::tls::Identity;
use mumble_server_runtime_gateway::{Gateway, GatewayConfig};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::TcpListenerStream;

use crate::actor;
use crate::config::{ConfigError, ControllerConfig};
use crate::core_protocol::controller_service_server::ControllerServiceServer as CoreServiceServer;
use crate::protocol::controller_service_server::ControllerServiceServer as LegacyServiceServer;
use crate::service::{ControllerRouter, service};

/// Both public listeners and their owned background tasks.
pub struct RunningControllerServer {
    controller_address: SocketAddr,
    mumble_address: SocketAddr,
    grpc_shutdown: Option<oneshot::Sender<()>>,
    grpc_task: Option<JoinHandle<Result<(), tonic::transport::Error>>>,
    gateway_task: Option<JoinHandle<Result<(), String>>>,
    actor_task: Option<JoinHandle<()>>,
}

impl RunningControllerServer {
    /// Bind gRPC and Mumble, then start the Controller actor and both serving planes.
    pub async fn start(
        config: ControllerConfig,
        identity: Identity,
    ) -> Result<Self, ServerStartError> {
        config.validate()?;
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

        let listener = TcpListener::bind(config.controller_bind)
            .await
            .map_err(ServerStartError::ControllerBind)?;
        let controller_address = listener
            .local_addr()
            .map_err(ServerStartError::ControllerAddress)?;

        let (actor, actor_task) = actor::spawn(config.clone(), runtime)?;
        let router = ControllerRouter::new(actor.clone());
        let gateway_task = tokio::spawn(async move {
            gateway
                .serve(router)
                .await
                .map_err(|error| error.to_string())
        });

        let controller_service = service(actor, config.queue_capacity);
        let core_grpc = CoreServiceServer::from_arc(controller_service.clone())
            .max_decoding_message_size(config.grpc_max_frame_bytes)
            .max_encoding_message_size(config.grpc_max_frame_bytes);
        let legacy_grpc = LegacyServiceServer::from_arc(controller_service)
            .max_decoding_message_size(config.grpc_max_frame_bytes)
            .max_encoding_message_size(config.grpc_max_frame_bytes);
        let (grpc_shutdown, shutdown) = oneshot::channel();
        let grpc_task = tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(core_grpc)
                .add_service(legacy_grpc)
                .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                    let _closed = shutdown.await;
                }),
        );

        Ok(Self {
            controller_address,
            mumble_address,
            grpc_shutdown: Some(grpc_shutdown),
            grpc_task: Some(grpc_task),
            gateway_task: Some(gateway_task),
            actor_task: Some(actor_task),
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
                    eprintln!("mumble-controller-server: gRPC server stopped: {error}")
                }
                Err(error) => eprintln!("mumble-controller-server: gRPC task failed: {error}"),
            }
        }
        if let Some(task) = &self.gateway_task {
            task.abort();
        }
        if let Some(task) = &self.actor_task {
            task.abort();
        }
        if let Some(task) = self.gateway_task.take() {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("mumble-controller-server: Mumble gateway stopped: {error}")
                }
                Err(error) if error.is_cancelled() => {}
                Err(error) => eprintln!("mumble-controller-server: Mumble task failed: {error}"),
            }
        }
        if let Some(task) = self.actor_task.take() {
            match task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => eprintln!("mumble-controller-server: actor task failed: {error}"),
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
}
