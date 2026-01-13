// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Broker availability and version compatibility checks.
//!
//! Verifies the broker is running and compatible before runtime initialization.

use crate::Result;
use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::{GetHealthRequest, GetVersionRequest};
use streamlib_broker::{GRPC_PORT, PROTOCOL_VERSION};

/// Expected protocol version. Must match the broker's PROTOCOL_VERSION.
const EXPECTED_PROTOCOL_VERSION: u32 = PROTOCOL_VERSION;

/// Check if the broker is available and version-compatible.
///
/// This function is called during `StreamRuntime::new()` on macOS to ensure
/// the broker service is running and compatible before proceeding.
///
/// # Errors
///
/// Returns an error if:
/// - The broker is not running (with instructions to install it)
/// - The broker's protocol version is incompatible (with upgrade instructions)
pub async fn check_broker_availability() -> Result<()> {
    let endpoint = format!("http://127.0.0.1:{}", GRPC_PORT);

    // Try to connect to the broker
    let mut client = match BrokerServiceClient::connect(endpoint.clone()).await {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!("Failed to connect to broker at {}: {}", endpoint, e);
            return Err(crate::StreamError::Runtime(format!(
                "StreamLib broker is not running.\n\n\
                 The broker is required for cross-process GPU resource sharing.\n\n\
                 To install and start the broker:\n\
                 \n\
                   streamlib broker install\n\
                 \n\
                 Or for development:\n\
                 \n\
                   ./scripts/dev-setup.sh\n\
                 \n\
                 After installation, the broker starts automatically on login."
            )));
        }
    };

    // Check health
    let health = client
        .get_health(GetHealthRequest {})
        .await
        .map_err(|e| {
            crate::StreamError::Runtime(format!(
                "Broker health check failed: {}. The broker may be starting up - try again.",
                e
            ))
        })?
        .into_inner();

    if !health.healthy {
        return Err(crate::StreamError::Runtime(format!(
            "Broker reports unhealthy status: {}",
            health.status
        )));
    }

    // Check version compatibility
    let version = client
        .get_version(GetVersionRequest {})
        .await
        .map_err(|e| crate::StreamError::Runtime(format!("Failed to get broker version: {}", e)))?
        .into_inner();

    if version.protocol_version != EXPECTED_PROTOCOL_VERSION {
        return Err(crate::StreamError::Runtime(format!(
            "Broker protocol version mismatch.\n\n\
             Runtime expects protocol v{}, but broker reports v{}.\n\n\
             This usually means the broker needs to be updated.\n\n\
             To update the broker:\n\
             \n\
               streamlib broker install --force\n\
             \n\
             Or for development:\n\
             \n\
               ./scripts/dev-setup.sh --clean",
            EXPECTED_PROTOCOL_VERSION, version.protocol_version
        )));
    }

    tracing::info!(
        "Broker v{} (protocol v{}) is healthy (uptime: {}s)",
        version.version,
        version.protocol_version,
        health.uptime_secs
    );

    Ok(())
}

/// Synchronous wrapper for broker check.
///
/// Creates a temporary tokio runtime to perform the async check.
/// Used during `StreamRuntime::new()` which may be called outside tokio context.
pub fn check_broker_availability_sync() -> Result<()> {
    // Try to use existing runtime handle if available
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        // We're inside tokio - use spawn_blocking to avoid blocking the runtime
        std::thread::scope(|s| {
            s.spawn(|| handle.block_on(check_broker_availability()))
                .join()
                .unwrap()
        })
    } else {
        // Not in tokio context - create a temporary runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                crate::StreamError::Runtime(format!(
                    "Failed to create runtime for broker check: {}",
                    e
                ))
            })?;
        rt.block_on(check_broker_availability())
    }
}
