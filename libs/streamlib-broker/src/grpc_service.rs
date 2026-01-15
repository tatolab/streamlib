// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! gRPC service implementation for broker diagnostics.

use tonic::{Request, Response, Status};

use crate::proto::broker_service_server::BrokerService;
use crate::proto::{
    ConnectionInfo, GetHealthRequest, GetHealthResponse, GetRuntimeEndpointRequest,
    GetRuntimeEndpointResponse, GetVersionRequest, GetVersionResponse, ListConnectionsRequest,
    ListConnectionsResponse, ListProcessorsRequest, ListProcessorsResponse, ListRuntimesRequest,
    ListRuntimesResponse, ProcessorInfo, PruneDeadRuntimesRequest, PruneDeadRuntimesResponse,
    RegisterRuntimeRequest, RegisterRuntimeResponse, RuntimeInfo, UnregisterRuntimeRequest,
    UnregisterRuntimeResponse,
};
use crate::state::BrokerState;

/// Current protocol version. Bump when gRPC API changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// gRPC service for broker diagnostics.
pub struct BrokerGrpcService {
    state: BrokerState,
}

impl BrokerGrpcService {
    /// Create a new gRPC service with shared state.
    pub fn new(state: BrokerState) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl BrokerService for BrokerGrpcService {
    async fn get_health(
        &self,
        _request: Request<GetHealthRequest>,
    ) -> Result<Response<GetHealthResponse>, Status> {
        let uptime = self.state.uptime_secs();

        Ok(Response::new(GetHealthResponse {
            healthy: true,
            status: "ok".to_string(),
            uptime_secs: uptime,
        }))
    }

    async fn get_version(
        &self,
        _request: Request<GetVersionRequest>,
    ) -> Result<Response<GetVersionResponse>, Status> {
        Ok(Response::new(GetVersionResponse {
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_commit: option_env!("GIT_COMMIT").unwrap_or("unknown").to_string(),
            build_date: option_env!("BUILD_DATE").unwrap_or("unknown").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    async fn list_runtimes(
        &self,
        _request: Request<ListRuntimesRequest>,
    ) -> Result<Response<ListRuntimesResponse>, Status> {
        let runtimes: Vec<RuntimeInfo> = self
            .state
            .get_runtimes()
            .into_iter()
            .map(|r| {
                let subprocess_count = self.state.subprocess_count_for_runtime(&r.runtime_id);
                let connection_count = self.state.connection_count_for_runtime(&r.runtime_id);
                RuntimeInfo {
                    runtime_id: r.runtime_id,
                    name: r.name,
                    api_endpoint: r.api_endpoint,
                    log_path: r.log_path,
                    pid: r.pid,
                    registered_at_unix_ms: r
                        .registered_at
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(0),
                    processor_count: subprocess_count as i32,
                    connection_count: connection_count as i32,
                }
            })
            .collect();

        Ok(Response::new(ListRuntimesResponse { runtimes }))
    }

    async fn list_processors(
        &self,
        request: Request<ListProcessorsRequest>,
    ) -> Result<Response<ListProcessorsResponse>, Status> {
        let runtime_id = &request.get_ref().runtime_id;

        let subprocesses = if runtime_id.is_empty() {
            self.state.get_subprocesses()
        } else {
            self.state.get_subprocesses_for_runtime(runtime_id)
        };

        let processors: Vec<ProcessorInfo> = subprocesses
            .into_iter()
            .map(|s| ProcessorInfo {
                runtime_id: s.runtime_id,
                processor_id: s.processor_id,
                processor_type: "SubprocessProcessor".to_string(), // Generic type for now
                registered_at_unix_ms: s
                    .registered_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(0),
                bridge_state: "connected".to_string(), // Assume connected if registered
            })
            .collect();

        Ok(Response::new(ListProcessorsResponse { processors }))
    }

    async fn list_connections(
        &self,
        request: Request<ListConnectionsRequest>,
    ) -> Result<Response<ListConnectionsResponse>, Status> {
        let runtime_id = &request.get_ref().runtime_id;

        let connection_metadata = if runtime_id.is_empty() {
            self.state.get_connections()
        } else {
            self.state.get_connections_for_runtime(runtime_id)
        };

        let connections: Vec<ConnectionInfo> = connection_metadata
            .into_iter()
            .map(|c| ConnectionInfo {
                connection_id: c.connection_id,
                runtime_id: c.runtime_id,
                processor_id: c.processor_id,
                role: c.role,
                established_at_unix_ms: c
                    .established_at
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(0),
                frames_transferred: 0, // Stats reported by runtime/subprocess (future)
                bytes_transferred: 0,  // Stats reported by runtime/subprocess (future)
            })
            .collect();

        Ok(Response::new(ListConnectionsResponse { connections }))
    }

    async fn get_runtime_endpoint(
        &self,
        request: Request<GetRuntimeEndpointRequest>,
    ) -> Result<Response<GetRuntimeEndpointResponse>, Status> {
        use crate::proto::get_runtime_endpoint_request::Query;

        let query = request.into_inner().query;

        let runtime = match query {
            Some(Query::Name(name)) => self.state.get_runtime_by_name(&name),
            Some(Query::RuntimeId(id)) => self.state.get_runtime_by_id(&id),
            None => {
                return Err(Status::invalid_argument(
                    "Query must specify name or runtime_id",
                ))
            }
        };

        match runtime {
            Some(r) => Ok(Response::new(GetRuntimeEndpointResponse {
                found: true,
                runtime_id: r.runtime_id,
                name: r.name,
                api_endpoint: r.api_endpoint,
                log_path: r.log_path,
            })),
            None => Ok(Response::new(GetRuntimeEndpointResponse {
                found: false,
                runtime_id: String::new(),
                name: String::new(),
                api_endpoint: String::new(),
                log_path: String::new(),
            })),
        }
    }

    async fn register_runtime(
        &self,
        request: Request<RegisterRuntimeRequest>,
    ) -> Result<Response<RegisterRuntimeResponse>, Status> {
        let req = request.into_inner();

        tracing::info!(
            "Registering runtime: {} (name: {}, pid: {}, api: {}, log: {})",
            req.runtime_id,
            req.name,
            req.pid,
            req.api_endpoint,
            req.log_path
        );

        self.state.register_runtime_with_metadata(
            &req.runtime_id,
            &req.name,
            &req.api_endpoint,
            &req.log_path,
            req.pid,
        );

        Ok(Response::new(RegisterRuntimeResponse {
            success: true,
            error: String::new(),
        }))
    }

    async fn unregister_runtime(
        &self,
        request: Request<UnregisterRuntimeRequest>,
    ) -> Result<Response<UnregisterRuntimeResponse>, Status> {
        let req = request.into_inner();

        tracing::info!("Unregistering runtime: {}", req.runtime_id);

        self.state.unregister_runtime(&req.runtime_id);

        Ok(Response::new(UnregisterRuntimeResponse { success: true }))
    }

    async fn prune_dead_runtimes(
        &self,
        _request: Request<PruneDeadRuntimesRequest>,
    ) -> Result<Response<PruneDeadRuntimesResponse>, Status> {
        let pruned_names = self.state.prune_dead_runtimes();
        let pruned_count = pruned_names.len() as i32;

        if pruned_count > 0 {
            tracing::info!(
                "Pruned {} dead runtime(s): {:?}",
                pruned_count,
                pruned_names
            );
        }

        Ok(Response::new(PruneDeadRuntimesResponse {
            pruned_count,
            pruned_names,
        }))
    }
}
