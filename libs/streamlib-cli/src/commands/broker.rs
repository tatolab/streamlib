// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Broker diagnostics commands.

use anyhow::{Context, Result};

use streamlib::broker_client::{
    BrokerServiceClient, GetHealthRequest, GetVersionRequest, ListConnectionsRequest,
    ListProcessorsRequest, ListRuntimesRequest, GRPC_PORT,
};

/// Get the broker gRPC endpoint.
fn broker_endpoint() -> String {
    format!("http://127.0.0.1:{}", GRPC_PORT)
}

/// Show broker health and version status.
pub async fn status() -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint.clone())
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    // Get health
    let health = client
        .get_health(GetHealthRequest {})
        .await
        .context("Failed to get broker health")?
        .into_inner();

    // Get version
    let version = client
        .get_version(GetVersionRequest {})
        .await
        .context("Failed to get broker version")?
        .into_inner();

    println!("Broker Status");
    println!("─────────────────────────────────");
    println!("  Endpoint:   {}", endpoint);
    println!(
        "  Health:     {}",
        if health.healthy {
            "healthy"
        } else {
            "unhealthy"
        }
    );
    println!("  Status:     {}", health.status);
    println!("  Uptime:     {}s", health.uptime_secs);
    println!();
    println!("Version Info");
    println!("─────────────────────────────────");
    println!("  Version:    {}", version.version);
    println!("  Git Commit: {}", version.git_commit);
    println!("  Build Date: {}", version.build_date);

    Ok(())
}

/// List registered runtimes.
pub async fn runtimes() -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .list_runtimes(ListRuntimesRequest {})
        .await
        .context("Failed to list runtimes")?
        .into_inner();

    if response.runtimes.is_empty() {
        println!("No runtimes registered.");
        return Ok(());
    }

    println!("Registered Runtimes ({}):", response.runtimes.len());
    println!("─────────────────────────────────────────────────────────");

    for runtime in &response.runtimes {
        println!("  Runtime: {}", runtime.runtime_id);
        println!("    Processors:  {}", runtime.processor_count);
        println!("    Connections: {}", runtime.connection_count);
        println!("    Age:         {}ms", runtime.registered_at_unix_ms);
        println!();
    }

    Ok(())
}

/// List registered processors.
pub async fn processors(runtime_id: Option<&str>) -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .list_processors(ListProcessorsRequest {
            runtime_id: runtime_id.unwrap_or("").to_string(),
        })
        .await
        .context("Failed to list processors")?
        .into_inner();

    if response.processors.is_empty() {
        if let Some(id) = runtime_id {
            println!("No processors registered for runtime '{}'.", id);
        } else {
            println!("No processors registered.");
        }
        return Ok(());
    }

    let title = if let Some(id) = runtime_id {
        format!(
            "Processors for Runtime '{}' ({}):",
            id,
            response.processors.len()
        )
    } else {
        format!("All Processors ({}):", response.processors.len())
    };

    println!("{}", title);
    println!("─────────────────────────────────────────────────────────");

    for proc in &response.processors {
        println!("  Processor: {}", proc.processor_id);
        println!("    Runtime: {}", proc.runtime_id);
        println!("    Type:    {}", proc.processor_type);
        println!("    State:   {}", proc.bridge_state);
        println!("    Age:     {}ms", proc.registered_at_unix_ms);
        println!();
    }

    Ok(())
}

/// List active connections.
pub async fn connections(runtime_id: Option<&str>) -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .list_connections(ListConnectionsRequest {
            runtime_id: runtime_id.unwrap_or("").to_string(),
        })
        .await
        .context("Failed to list connections")?
        .into_inner();

    if response.connections.is_empty() {
        if let Some(id) = runtime_id {
            println!("No connections for runtime '{}'.", id);
        } else {
            println!("No active connections.");
        }
        return Ok(());
    }

    let title = if let Some(id) = runtime_id {
        format!(
            "Connections for Runtime '{}' ({}):",
            id,
            response.connections.len()
        )
    } else {
        format!("All Connections ({}):", response.connections.len())
    };

    println!("{}", title);
    println!("─────────────────────────────────────────────────────────");

    for conn in &response.connections {
        println!("  Connection: {}", conn.connection_id);
        println!("    Runtime:   {}", conn.runtime_id);
        println!("    Processor: {}", conn.processor_id);
        println!("    Role:      {}", conn.role);
        println!("    Age:       {}ms", conn.established_at_unix_ms);
        if conn.frames_transferred > 0 || conn.bytes_transferred > 0 {
            println!("    Frames:    {}", conn.frames_transferred);
            println!("    Bytes:     {}", conn.bytes_transferred);
        }
        println!();
    }

    Ok(())
}
