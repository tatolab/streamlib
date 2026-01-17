// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runtime discovery and listing commands.

use anyhow::{Context, Result};

use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::{ListRuntimesRequest, PruneDeadRuntimesRequest};
use streamlib_broker::GRPC_PORT;

/// Get the broker gRPC endpoint.
fn broker_endpoint() -> String {
    let port = std::env::var("STREAMLIB_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(GRPC_PORT);
    format!("http://127.0.0.1:{}", port)
}

/// Format duration in human-readable form (kubectl style: 5s, 2m30s, 1h5m, 2d3h).
fn format_age(millis: i64) -> String {
    let secs = millis / 1000;
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let mins = secs / 60;
        let remaining_secs = secs % 60;
        if remaining_secs > 0 {
            format!("{}m{}s", mins, remaining_secs)
        } else {
            format!("{}m", mins)
        }
    } else if secs < 86400 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins > 0 {
            format!("{}h{}m", hours, mins)
        } else {
            format!("{}h", hours)
        }
    } else {
        let days = secs / 86400;
        let hours = (secs % 86400) / 3600;
        if hours > 0 {
            format!("{}d{}h", days, hours)
        } else {
            format!("{}d", days)
        }
    }
}

/// Check if a process is alive using kill(pid, 0).
/// Signal 0 doesn't send any signal - it just checks if the process exists.
fn is_process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill with signal 0 is safe - it only checks process existence
    unsafe { libc::kill(pid, 0) == 0 }
}

/// List all registered runtimes.
pub async fn list() -> Result<()> {
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

    // kubectl-style output
    println!("{:<20} {:>8} {:<10} {:>8}", "NAME", "PID", "STATUS", "AGE");

    for runtime in &response.runtimes {
        let name = if runtime.name.is_empty() {
            &runtime.runtime_id
        } else {
            &runtime.name
        };
        let age = format_age(runtime.registered_at_unix_ms);
        let status = if is_process_alive(runtime.pid) {
            "Running"
        } else {
            "Dead"
        };
        println!(
            "{:<20} {:>8} {:<10} {:>8}",
            truncate(name, 20),
            runtime.pid,
            status,
            age,
        );
    }

    Ok(())
}

/// Truncate a string to max length with ellipsis.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

/// Prune dead runtimes from the broker.
pub async fn prune() -> Result<()> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let response = client
        .prune_dead_runtimes(PruneDeadRuntimesRequest {})
        .await
        .context("Failed to prune dead runtimes")?
        .into_inner();

    if response.pruned_count == 0 {
        println!("No dead runtimes to prune.");
    } else {
        println!("Pruned {} runtime(s):", response.pruned_count);
        for name in &response.pruned_names {
            println!("  - {}", name);
        }
    }

    Ok(())
}
