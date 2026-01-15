// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! gRPC service implementation for broker diagnostics.

use tonic::{Request, Response, Status};

use std::time::Instant;

use crate::proto::broker_service_server::BrokerService;
use crate::proto::{
    // Phase 4 XPC Bridge messages
    AllocateConnectionRequest,
    AllocateConnectionResponse,
    ClientAliveRequest,
    ClientAliveResponse,
    CloseConnectionRequest,
    CloseConnectionResponse,
    // Existing Phase 3 messages
    ConnectionInfo,
    GetClientStatusRequest,
    GetClientStatusResponse,
    GetConnectionInfoRequest,
    GetConnectionInfoResponse,
    GetHealthRequest,
    GetHealthResponse,
    GetHostStatusRequest,
    GetHostStatusResponse,
    GetRuntimeEndpointRequest,
    GetRuntimeEndpointResponse,
    GetVersionRequest,
    GetVersionResponse,
    HostAliveRequest,
    HostAliveResponse,
    HostXpcReadyRequest,
    HostXpcReadyResponse,
    ListConnectionsRequest,
    ListConnectionsResponse,
    ListProcessorsRequest,
    ListProcessorsResponse,
    ListRuntimesRequest,
    ListRuntimesResponse,
    MarkAckedRequest,
    MarkAckedResponse,
    ProcessorInfo,
    PruneDeadRuntimesRequest,
    PruneDeadRuntimesResponse,
    RegisterRuntimeRequest,
    RegisterRuntimeResponse,
    RuntimeInfo,
    UnregisterRuntimeRequest,
    UnregisterRuntimeResponse,
};
use crate::state::{BrokerState, ClientState, HostState};

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

    // ─────────────────────────────────────────────────────────────────────────
    // XPC Bridge Connection Management (Phase 4)
    // ─────────────────────────────────────────────────────────────────────────

    async fn allocate_connection(
        &self,
        request: Request<AllocateConnectionRequest>,
    ) -> Result<Response<AllocateConnectionResponse>, Status> {
        let req = request.into_inner();

        let connection_id = self
            .state
            .allocate_xpc_bridge_connection(&req.runtime_id, &req.processor_id);

        tracing::info!(
            "Allocated XPC bridge connection: {} (runtime: {}, processor: {})",
            connection_id,
            req.runtime_id,
            req.processor_id
        );

        Ok(Response::new(AllocateConnectionResponse {
            connection_id,
            success: true,
            error: String::new(),
        }))
    }

    async fn host_alive(
        &self,
        request: Request<HostAliveRequest>,
    ) -> Result<Response<HostAliveResponse>, Status> {
        let req = request.into_inner();

        let updated = self
            .state
            .update_xpc_bridge_connection(&req.connection_id, |conn| {
                conn.host_state = HostState::Alive;
                conn.host_alive_at = Some(Instant::now());
                conn.host_last_seen = Instant::now();
            });

        if !updated {
            return Ok(Response::new(HostAliveResponse {
                success: false,
                client_state: String::new(),
            }));
        }

        let client_state = self
            .state
            .get_xpc_bridge_connection(&req.connection_id)
            .map(|c| c.client_state.as_str().to_string())
            .unwrap_or_default();

        tracing::debug!(
            "Host alive for connection: {}, client_state: {}",
            req.connection_id,
            client_state
        );

        Ok(Response::new(HostAliveResponse {
            success: true,
            client_state,
        }))
    }

    async fn host_xpc_ready(
        &self,
        request: Request<HostXpcReadyRequest>,
    ) -> Result<Response<HostXpcReadyResponse>, Status> {
        let req = request.into_inner();

        let updated = self
            .state
            .update_xpc_bridge_connection(&req.connection_id, |conn| {
                conn.host_state = HostState::XpcReady;
                conn.host_xpc_endpoint_stored = true;
                conn.host_xpc_endpoint_stored_at = Some(Instant::now());
                conn.host_last_seen = Instant::now();
            });

        if !updated {
            return Ok(Response::new(HostXpcReadyResponse {
                success: false,
                client_state: String::new(),
            }));
        }

        let client_state = self
            .state
            .get_xpc_bridge_connection(&req.connection_id)
            .map(|c| c.client_state.as_str().to_string())
            .unwrap_or_default();

        tracing::info!(
            "Host XPC ready for connection: {}, client_state: {}",
            req.connection_id,
            client_state
        );

        Ok(Response::new(HostXpcReadyResponse {
            success: true,
            client_state,
        }))
    }

    async fn client_alive(
        &self,
        request: Request<ClientAliveRequest>,
    ) -> Result<Response<ClientAliveResponse>, Status> {
        let req = request.into_inner();

        let updated = self
            .state
            .update_xpc_bridge_connection(&req.connection_id, |conn| {
                conn.client_state = ClientState::Alive;
                conn.client_alive_at = Some(Instant::now());
                conn.client_last_seen = Instant::now();
            });

        if !updated {
            return Ok(Response::new(ClientAliveResponse {
                success: false,
                host_state: String::new(),
            }));
        }

        let host_state = self
            .state
            .get_xpc_bridge_connection(&req.connection_id)
            .map(|c| c.host_state.as_str().to_string())
            .unwrap_or_default();

        tracing::debug!(
            "Client alive for connection: {}, host_state: {}",
            req.connection_id,
            host_state
        );

        Ok(Response::new(ClientAliveResponse {
            success: true,
            host_state,
        }))
    }

    async fn get_client_status(
        &self,
        request: Request<GetClientStatusRequest>,
    ) -> Result<Response<GetClientStatusResponse>, Status> {
        let req = request.into_inner();

        // Update host_last_seen while polling
        self.state
            .update_xpc_bridge_connection(&req.connection_id, |conn| {
                conn.host_last_seen = Instant::now();
            });

        let client_state = self
            .state
            .get_xpc_bridge_connection(&req.connection_id)
            .map(|c| c.client_state.as_str().to_string())
            .unwrap_or_else(|| "not_found".to_string());

        Ok(Response::new(GetClientStatusResponse { client_state }))
    }

    async fn get_host_status(
        &self,
        request: Request<GetHostStatusRequest>,
    ) -> Result<Response<GetHostStatusResponse>, Status> {
        let req = request.into_inner();

        // Update client_last_seen while polling
        self.state
            .update_xpc_bridge_connection(&req.connection_id, |conn| {
                conn.client_last_seen = Instant::now();
            });

        let host_state = self
            .state
            .get_xpc_bridge_connection(&req.connection_id)
            .map(|c| c.host_state.as_str().to_string())
            .unwrap_or_else(|| "not_found".to_string());

        Ok(Response::new(GetHostStatusResponse { host_state }))
    }

    async fn mark_acked(
        &self,
        request: Request<MarkAckedRequest>,
    ) -> Result<Response<MarkAckedResponse>, Status> {
        let req = request.into_inner();

        let updated = self
            .state
            .update_xpc_bridge_connection(&req.connection_id, |conn| {
                let now = Instant::now();
                match req.side.as_str() {
                    "host" => {
                        conn.host_state = HostState::Acked;
                        conn.host_acked_at = Some(now);
                        conn.host_last_seen = now;
                    }
                    "client" => {
                        conn.client_state = ClientState::Acked;
                        conn.client_acked_at = Some(now);
                        conn.client_last_seen = now;
                    }
                    _ => {}
                }

                // Check if both sides are acked → connection ready
                if matches!(conn.host_state, HostState::Acked)
                    && matches!(conn.client_state, ClientState::Acked)
                {
                    conn.ready_at = Some(now);
                }
            });

        if !updated {
            return Ok(Response::new(MarkAckedResponse {
                success: false,
                connection_state: "not_found".to_string(),
            }));
        }

        let connection_state = self
            .state
            .get_xpc_bridge_connection(&req.connection_id)
            .map(|c| c.derived_state().as_str().to_string())
            .unwrap_or_else(|| "not_found".to_string());

        tracing::info!(
            "Marked {} acked for connection: {}, state: {}",
            req.side,
            req.connection_id,
            connection_state
        );

        Ok(Response::new(MarkAckedResponse {
            success: true,
            connection_state,
        }))
    }

    async fn get_connection_info(
        &self,
        request: Request<GetConnectionInfoRequest>,
    ) -> Result<Response<GetConnectionInfoResponse>, Status> {
        let req = request.into_inner();

        match self.state.get_xpc_bridge_connection(&req.connection_id) {
            Some(conn) => Ok(Response::new(GetConnectionInfoResponse {
                found: true,
                host_state: conn.host_state.as_str().to_string(),
                client_state: conn.client_state.as_str().to_string(),
                derived_state: conn.derived_state().as_str().to_string(),
                host_xpc_endpoint_stored: conn.host_xpc_endpoint_stored,
                client_connected: conn.client_endpoint_delivered,
                age_secs: conn.age_secs() as i64,
                timeout_secs: conn.timeout_secs as i64,
            })),
            None => Ok(Response::new(GetConnectionInfoResponse {
                found: false,
                host_state: String::new(),
                client_state: String::new(),
                derived_state: String::new(),
                host_xpc_endpoint_stored: false,
                client_connected: false,
                age_secs: 0,
                timeout_secs: 0,
            })),
        }
    }

    async fn close_connection(
        &self,
        request: Request<CloseConnectionRequest>,
    ) -> Result<Response<CloseConnectionResponse>, Status> {
        let req = request.into_inner();

        let removed = self.state.remove_xpc_bridge_connection(&req.connection_id);

        if removed.is_some() {
            tracing::info!(
                "Closed XPC bridge connection: {} (reason: {})",
                req.connection_id,
                req.reason
            );
        }

        Ok(Response::new(CloseConnectionResponse {
            success: removed.is_some(),
        }))
    }
}
