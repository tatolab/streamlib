// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib Broker Service
//!
//! This binary runs as a system service (launchd on macOS, systemd on Linux), providing:
//! - gRPC interface for runtime tracking and diagnostics
//! - Telemetry ingestion (single SQLite writer for all runtimes)

use std::sync::Arc;

use clap::Parser;
use tracing::info;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use streamlib_broker::{
    proto::broker_service_server::BrokerServiceServer, BrokerGrpcService, BrokerState,
};

#[cfg(target_os = "macos")]
use streamlib_broker::XpcSurfaceService;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use streamlib_telemetry::proto::telemetry_ingest_service_server::TelemetryIngestServiceServer;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use streamlib_telemetry::sqlite_telemetry_database::SqliteTelemetryDatabase;

#[derive(Parser)]
#[command(name = "streamlib-broker")]
#[command(about = "StreamLib broker service for runtime coordination")]
struct Cli {
    /// Port for the gRPC server
    #[arg(long, default_value_t = streamlib_broker::GRPC_PORT)]
    port: u16,

    /// XPC service name for surface store (from STREAMLIB_XPC_SERVICE_NAME env var)
    #[cfg(target_os = "macos")]
    #[arg(long, env = "STREAMLIB_XPC_SERVICE_NAME")]
    xpc_service_name: Option<String>,

    /// Unix socket path for surface store (from STREAMLIB_BROKER_SOCKET env var)
    #[cfg(target_os = "linux")]
    #[arg(long, env = "STREAMLIB_BROKER_SOCKET")]
    socket_path: Option<String>,
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize telemetry for the broker's OWN logs (SQLite path, no broker_endpoint)
    let _telemetry_guard =
        streamlib_telemetry::init_telemetry(streamlib_telemetry::TelemetryConfig {
            service_name: "streamlib-broker".into(),
            resource_attributes: vec![("process.pid".into(), std::process::id().to_string())],
            file_log_path: None,
            stdout_logging: true,
            otlp_endpoint: std::env::var("STREAMLIB_OTLP_ENDPOINT").ok(),
            sqlite_database_path: None,
            broker_endpoint: None, // Broker IS the collector — writes to SQLite directly
        })
        .expect("Failed to initialize telemetry");

    info!(
        "[Broker] Starting StreamLib broker service v{} (PID: {})",
        streamlib_broker::VERSION,
        std::process::id()
    );

    // Open the telemetry database for the IngestTelemetry handler
    let telemetry_db_path =
        streamlib_telemetry::sqlite_telemetry_database::default_telemetry_database_path()
            .expect("Failed to determine telemetry database path");
    let telemetry_database = Arc::new(
        SqliteTelemetryDatabase::open(&telemetry_db_path)
            .expect("Failed to open telemetry database for ingestion"),
    );

    // Create shared state for diagnostics
    let state = BrokerState::new();

    // Start XPC surface service if service name is provided
    let _xpc_service = if let Some(ref xpc_service_name) = cli.xpc_service_name {
        let mut xpc_service = XpcSurfaceService::new(state.clone(), xpc_service_name.clone());
        match xpc_service.start() {
            Ok(()) => {
                info!(
                    "[Broker] XPC surface service started on '{}'",
                    xpc_service_name
                );
                Some(xpc_service)
            }
            Err(e) => {
                tracing::error!("[Broker] Failed to start XPC surface service: {}", e);
                // Continue without XPC service - gRPC still works
                None
            }
        }
    } else {
        info!("[Broker] XPC surface service disabled (no STREAMLIB_XPC_SERVICE_NAME set)");
        None
    };

    // Start periodic cleanup thread (prunes dead runtimes every 30 seconds)
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;

            let pruned = cleanup_state.prune_dead_runtimes();
            if !pruned.is_empty() {
                tracing::info!(
                    "[Broker] Pruned {} dead runtime(s): {:?}",
                    pruned.len(),
                    pruned
                );
            }
        }
    });

    // Start periodic telemetry pruning (deletes entries older than 7 days, every hour)
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            match streamlib_telemetry::prune_old_telemetry(7) {
                Ok(0) => {}
                Ok(n) => tracing::info!("[Broker] Pruned {} old telemetry record(s)", n),
                Err(e) => tracing::warn!("[Broker] Telemetry prune failed: {}", e),
            }
        }
    });

    // Start gRPC server (blocks forever)
    if let Err(e) = start_grpc_server(state, telemetry_database, cli.port).await {
        tracing::error!("[Broker] gRPC server error: {}", e);
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    use streamlib_broker::unix_socket_service::UnixSocketSurfaceService;

    let cli = Cli::parse();

    // Initialize telemetry for the broker's OWN logs
    let _telemetry_guard =
        streamlib_telemetry::init_telemetry(streamlib_telemetry::TelemetryConfig {
            service_name: "streamlib-broker".into(),
            resource_attributes: vec![("process.pid".into(), std::process::id().to_string())],
            file_log_path: None,
            stdout_logging: true,
            otlp_endpoint: std::env::var("STREAMLIB_OTLP_ENDPOINT").ok(),
            sqlite_database_path: None,
            broker_endpoint: None,
        })
        .expect("Failed to initialize telemetry");

    info!(
        "[Broker] Starting StreamLib broker service v{} (PID: {})",
        streamlib_broker::VERSION,
        std::process::id()
    );

    // Open the telemetry database for the IngestTelemetry handler
    let telemetry_db_path =
        streamlib_telemetry::sqlite_telemetry_database::default_telemetry_database_path()
            .expect("Failed to determine telemetry database path");
    let telemetry_database = Arc::new(
        SqliteTelemetryDatabase::open(&telemetry_db_path)
            .expect("Failed to open telemetry database for ingestion"),
    );

    // Create shared state for diagnostics
    let state = BrokerState::new();

    // Determine socket path
    let socket_path = cli.socket_path.unwrap_or_else(|| {
        let streamlib_home = std::env::var("STREAMLIB_HOME")
            .unwrap_or_else(|_| format!("{}/.streamlib", std::env::var("HOME").unwrap()));
        format!("{}/broker.sock", streamlib_home)
    });

    // Start Unix socket surface service
    let mut unix_socket_service =
        UnixSocketSurfaceService::new(state.clone(), std::path::PathBuf::from(&socket_path));
    match unix_socket_service.start() {
        Ok(()) => {
            info!(
                "[Broker] Unix socket surface service started on '{}'",
                socket_path
            );
        }
        Err(e) => {
            tracing::error!("[Broker] Failed to start Unix socket service: {}", e);
            // Continue without surface service - gRPC still works
        }
    }

    // Start periodic cleanup thread (prunes dead runtimes every 30 seconds)
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;

            let pruned = cleanup_state.prune_dead_runtimes();
            if !pruned.is_empty() {
                tracing::info!(
                    "[Broker] Pruned {} dead runtime(s): {:?}",
                    pruned.len(),
                    pruned
                );
            }
        }
    });

    // Start periodic telemetry pruning (deletes entries older than 7 days, every hour)
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            match streamlib_telemetry::prune_old_telemetry(7) {
                Ok(0) => {}
                Ok(n) => tracing::info!("[Broker] Pruned {} old telemetry record(s)", n),
                Err(e) => tracing::warn!("[Broker] Telemetry prune failed: {}", e),
            }
        }
    });

    // Start gRPC server (blocks forever)
    // Keep _unix_socket_service alive so it doesn't get dropped
    let _unix_socket_service = unix_socket_service;
    if let Err(e) = start_grpc_server(state, telemetry_database, cli.port).await {
        tracing::error!("[Broker] gRPC server error: {}", e);
        std::process::exit(1);
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn start_grpc_server(
    state: BrokerState,
    telemetry_database: Arc<SqliteTelemetryDatabase>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use opentelemetry_otlp::WithExportConfig;
    use tonic::transport::Server;

    let addr = format!("127.0.0.1:{}", port).parse()?;

    // Create OTLP span exporter if endpoint is configured
    let otlp_span_exporter = if let Ok(endpoint) = std::env::var("STREAMLIB_OTLP_ENDPOINT") {
        info!("[Broker] OTLP forwarding enabled → {}", endpoint);
        Some(
            opentelemetry_otlp::SpanExporter::builder()
                .with_tonic()
                .with_endpoint(&endpoint)
                .build()?,
        )
    } else {
        None
    };

    let service = Arc::new(BrokerGrpcService::new(
        state,
        telemetry_database,
        otlp_span_exporter,
    ));

    info!(
        "[Broker] Starting gRPC server on {} (BrokerService + TelemetryIngestService)",
        addr
    );

    Server::builder()
        .add_service(BrokerServiceServer::from_arc(service.clone()))
        .add_service(TelemetryIngestServiceServer::from_arc(service))
        .serve(addr)
        .await?;

    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() {
    eprintln!("StreamLib broker is only supported on macOS and Linux.");
    std::process::exit(1);
}
