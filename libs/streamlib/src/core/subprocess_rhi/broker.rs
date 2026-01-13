// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess broker trait for cross-process endpoint exchange.

use crate::core::error::StreamError;

/// Result of broker installation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerInstallStatus {
    /// Broker was already running (found via launchctl list).
    AlreadyRunning,
    /// Broker was installed by this runtime (first runtime wins).
    Installed,
    /// Broker is not required on this platform (Linux/Windows).
    NotRequired,
}

impl BrokerInstallStatus {
    /// Returns true if the broker is now available (either already running or just installed).
    pub fn is_available(&self) -> bool {
        matches!(
            self,
            Self::AlreadyRunning | Self::Installed | Self::NotRequired
        )
    }
}

/// Broker trait for managing runtime endpoint registration and lookup.
///
/// On macOS, this is implemented via XPC launchd service.
/// On Linux/Windows, this is a no-op (direct socket/pipe connections).
pub trait SubprocessRhiBroker: Send + Sync {
    /// Ensure the broker service is running.
    ///
    /// On macOS:
    /// - Checks if `com.tatolab.streamlib.runtime` launchd service exists
    /// - If not, generates plist and bootstraps the service
    /// - First runtime to call this installs the broker
    ///
    /// On other platforms:
    /// - Returns `NotRequired` immediately
    fn ensure_running() -> Result<BrokerInstallStatus, StreamError>
    where
        Self: Sized;

    /// Register this runtime's endpoint with the broker.
    ///
    /// Called by the host runtime to advertise its XPC endpoint.
    /// The endpoint can then be retrieved by subprocesses via `get_endpoint`.
    fn register_endpoint(
        &self,
        runtime_id: &str,
        endpoint: *mut std::ffi::c_void,
    ) -> Result<(), StreamError>;

    /// Get a runtime's endpoint from the broker.
    ///
    /// Called by subprocesses to establish direct connection to a runtime.
    /// Returns the XPC endpoint that can be used with `xpc_connection_create_from_endpoint`.
    fn get_endpoint(&self, runtime_id: &str) -> Result<*mut std::ffi::c_void, StreamError>;

    /// Unregister this runtime's endpoint from the broker.
    ///
    /// Called during runtime shutdown for cleanup.
    fn unregister_endpoint(&self, runtime_id: &str) -> Result<(), StreamError>;
}
