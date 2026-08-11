use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use mumble_server_runtime_gateway::{ConnectionIdentity, ConnectionRouter, RouteDecision};
use mumble_server_runtime_shard::ConnectionId;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};

use crate::actor::{ActorCommand, ActorHandle};
use crate::core_protocol::controller_service_server::ControllerService as CoreControllerService;
use crate::core_protocol::{ClientFrame as CoreClientFrame, ServerFrame as CoreServerFrame};
use crate::wire::{from_core_frame, to_core_frame};

pub(crate) struct GrpcControllerService {
    actor: ActorHandle,
    queue_capacity: usize,
    next_stream_id: AtomicU64,
}

impl GrpcControllerService {
    pub(crate) fn new(actor: ActorHandle, queue_capacity: usize) -> Self {
        Self {
            actor,
            queue_capacity,
            next_stream_id: AtomicU64::new(1),
        }
    }
}

#[tonic::async_trait]
impl CoreControllerService for GrpcControllerService {
    type ConnectStream =
        Pin<Box<dyn Stream<Item = Result<CoreServerFrame, Status>> + Send + 'static>>;

    async fn connect(
        &self,
        request: Request<Streaming<CoreClientFrame>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::Relaxed);
        let mut incoming = request.into_inner();
        let (responses, outgoing) = mpsc::channel(self.queue_capacity);
        let actor = self.actor.sender();

        // Detached intentionally: the task owns exactly one gRPC request stream and terminates
        // when that stream closes. Actor state is notified in its tail.
        tokio::spawn(async move {
            let first = match incoming.message().await {
                Ok(Some(frame)) => match from_core_frame(frame) {
                    Ok(frame) => frame,
                    Err(status) => {
                        let _result = responses.send(Err(status)).await;
                        return;
                    }
                },
                Ok(None) => {
                    let _result = responses
                        .send(Err(Status::invalid_argument("Controller stream is empty")))
                        .await;
                    return;
                }
                Err(status) => {
                    let _result = responses.send(Err(status)).await;
                    return;
                }
            };
            if actor
                .send(ActorCommand::Open {
                    stream_id,
                    responses: responses.clone(),
                    frame: first,
                })
                .await
                .is_err()
            {
                let _result = responses
                    .send(Err(Status::unavailable("Controller actor stopped")))
                    .await;
                return;
            }

            loop {
                match incoming.message().await {
                    Ok(Some(frame)) => {
                        let frame = match from_core_frame(frame) {
                            Ok(frame) => frame,
                            Err(status) => {
                                let _result = responses.send(Err(status)).await;
                                return;
                            }
                        };
                        if actor
                            .send(ActorCommand::Frame { stream_id, frame })
                            .await
                            .is_err()
                        {
                            let _result = responses
                                .send(Err(Status::unavailable("Controller actor stopped")))
                                .await;
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(_status) => break,
                }
            }
            let _result = actor.send(ActorCommand::StreamClosed { stream_id }).await;
        });

        let outgoing = ReceiverStream::new(outgoing)
            .map(|result| result.and_then(|frame| to_core_frame(frame).map_err(Status::internal)));
        Ok(Response::new(Box::pin(outgoing)))
    }
}

#[derive(Clone)]
pub(crate) struct ControllerRouter {
    actor: ActorHandle,
}

impl ControllerRouter {
    pub(crate) fn new(actor: ActorHandle) -> Self {
        Self { actor }
    }
}

impl ConnectionRouter for ControllerRouter {
    async fn route(
        &self,
        connection: ConnectionId,
        identity: &ConnectionIdentity,
    ) -> RouteDecision {
        let (response, awaited) = oneshot::channel();
        let command = ActorCommand::Route {
            connection,
            credential: identity.credential.clone(),
            response,
        };
        if self.actor.sender().send(command).await.is_err() {
            return RouteDecision::Reject("the Controller service is unavailable".to_owned());
        }
        match awaited.await {
            Ok(Ok(shard)) => RouteDecision::Attach(shard),
            Ok(Err(reason)) => RouteDecision::Reject(reason),
            Err(_closed) => {
                RouteDecision::Reject("the Controller service is unavailable".to_owned())
            }
        }
    }
}

pub(crate) fn service(actor: ActorHandle, queue_capacity: usize) -> Arc<GrpcControllerService> {
    Arc::new(GrpcControllerService::new(actor, queue_capacity))
}
