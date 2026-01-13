// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! gRPC service implementation for broker diagnostics.

use tonic::{Request, Response, Status};

use super::broker_state::BrokerState;
use super::proto::broker_service_server::BrokerService;
use super::proto::{
    ConnectionInfo, GetHealthRequest, GetHealthResponse, GetVersionRequest, GetVersionResponse,
    ListConnectionsRequest, ListConnectionsResponse, ListProcessorsRequest, ListProcessorsResponse,
    ListRuntimesRequest, ListRuntimesResponse, ProcessorInfo, RuntimeInfo,
};

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
                RuntimeInfo {
                    runtime_id: r.runtime_id,
                    registered_at_unix_ms: r
                        .registered_at
                        .elapsed()
                        .as_millis()
                        .try_into()
                        .unwrap_or(0),
                    processor_count: subprocess_count as i32,
                    connection_count: 0, // M2.5 will implement connection tracking
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
        _request: Request<ListConnectionsRequest>,
    ) -> Result<Response<ListConnectionsResponse>, Status> {
        // M2.5: Will implement with actual connection data
        let connections: Vec<ConnectionInfo> = vec![];
        Ok(Response::new(ListConnectionsResponse { connections }))
    }
}
