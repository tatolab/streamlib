// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! gRPC service implementation for broker diagnostics.

use std::time::Instant;

use tonic::{Request, Response, Status};

use super::proto::broker_service_server::BrokerService;
use super::proto::{
    ConnectionInfo, GetHealthRequest, GetHealthResponse, GetVersionRequest, GetVersionResponse,
    ListConnectionsRequest, ListConnectionsResponse, ListProcessorsRequest, ListProcessorsResponse,
    ListRuntimesRequest, ListRuntimesResponse, ProcessorInfo, RuntimeInfo,
};

/// gRPC service for broker diagnostics.
pub struct BrokerGrpcService {
    /// Broker start time for uptime calculation.
    started_at: Instant,
}

impl BrokerGrpcService {
    /// Create a new gRPC service.
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
        }
    }
}

#[tonic::async_trait]
impl BrokerService for BrokerGrpcService {
    async fn get_health(
        &self,
        _request: Request<GetHealthRequest>,
    ) -> Result<Response<GetHealthResponse>, Status> {
        let uptime = self.started_at.elapsed().as_secs() as i64;

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
        // M2.4: Will implement with actual runtime data
        let runtimes: Vec<RuntimeInfo> = vec![];
        Ok(Response::new(ListRuntimesResponse { runtimes }))
    }

    async fn list_processors(
        &self,
        _request: Request<ListProcessorsRequest>,
    ) -> Result<Response<ListProcessorsResponse>, Status> {
        // M2.4: Will implement with actual processor data
        let processors: Vec<ProcessorInfo> = vec![];
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
