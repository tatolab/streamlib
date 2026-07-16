// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-free runtime-control requests a plugin can make of its host.
//!
//! A plugin authored against this SDK links no engine, so it holds no
//! engine `Event` / `RuntimeEvent` type and cannot publish a shutdown
//! event directly. Instead it publishes a msgpack reason string to the
//! reserved plugin-ABI control topic
//! ([`streamlib_plugin_abi::PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST`])
//! through the cached `pubsub_publish` callback; the host — the only
//! party that owns `Event` — maps the request onto its internal
//! runtime-shutdown event. Only the plugin-ABI topic constant and a
//! reason string cross the boundary, never an engine type.

use streamlib_error::{Error, Result};
use streamlib_plugin_abi::PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST;

/// Ask the host runtime to shut down (equivalent to Ctrl+C / SIGTERM).
///
/// `reason` is a human-readable attribution the host logs (empty string
/// = unspecified). Idempotent and fire-and-forget: the host maps this
/// onto its internal runtime-shutdown event, repeated calls are
/// harmless, and a call during teardown is a no-op on the host side.
///
/// Returns [`Error::PluginHostUnavailable`] when called from a cdylib
/// whose host services were never installed — i.e. code not loaded by a
/// streamlib host — which is the only real failure class.
#[tracing::instrument]
pub fn request_runtime_shutdown(reason: &str) -> Result<()> {
    let Some(callbacks) = crate::plugin::host_callbacks() else {
        return Err(Error::PluginHostUnavailable(
            "request_runtime_shutdown called in a cdylib whose host services \
             were never installed (not loaded by a streamlib host)"
                .into(),
        ));
    };

    let reason_msgpack = rmp_serde::to_vec(reason)
        .map_err(|e| Error::Runtime(format!("failed to encode shutdown reason: {e}")))?;

    // SAFETY: `callbacks.pubsub_publish` and `callbacks.host` were
    // populated by `install_host_services` from a host-provided
    // `HostServices` and stay valid for the plugin's process lifetime.
    // The topic and payload slices outlive the synchronous call.
    unsafe {
        (callbacks.pubsub_publish)(
            callbacks.host,
            PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST.as_ptr(),
            PUBSUB_CONTROL_TOPIC_RUNTIME_SHUTDOWN_REQUEST.len(),
            reason_msgpack.as_ptr(),
            reason_msgpack.len(),
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no host services installed (the SDK lib test binary never
    /// calls `install_host_services`), the request has no transport to
    /// reach and must surface the named `PluginHostUnavailable` error
    /// rather than silently succeeding.
    #[test]
    fn request_runtime_shutdown_without_host_returns_plugin_host_unavailable() {
        let result = request_runtime_shutdown("no host installed");
        assert!(
            matches!(result, Err(Error::PluginHostUnavailable(_))),
            "expected PluginHostUnavailable, got {result:?}",
        );
    }
}
