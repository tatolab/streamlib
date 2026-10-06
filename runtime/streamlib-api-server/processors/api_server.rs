// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `ApiServer` processor — owns the local API socket's lifecycle and binds
//! the shared `AppState` to per-request handlers.

use std::path::PathBuf;
use std::sync::Arc;

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::runtime::{RuntimeOperations, RuntimeUniqueId};

/// Handles cloned from the setup-time context for use in start().
/// The `tokio_handle` points at this processor's own tokio runtime
/// (constructed in `setup()`) — the processor owns a dedicated runtime
/// rather than assuming the lifecycle thread that calls `setup` / `start`
/// is itself inside one.
struct StashedHandles {
    runtime: Arc<dyn RuntimeOperations>,
    tokio_handle: tokio::runtime::Handle,
    runtime_id: String,
    local_api_socket_path: PathBuf,
}

#[streamlib::sdk::processor(
    description = "Runtime API server — HTTP + WebSocket control plane",
    execution = manual,
    config = crate::ApiServerConfig,
)]
pub struct ApiServerProcessor {
    handles: Option<StashedHandles>,
    /// Processor-owned tokio runtime. Constructed in `setup()`, dropped in
    /// `teardown()`. axum / hyper / tokio::net all run inside this runtime
    /// — their reactor / timer thread-local state is set when this
    /// runtime's worker threads enter it, so the HTTP server never depends
    /// on the calling lifecycle thread already being inside a tokio runtime.
    tokio_runtime: Option<tokio::runtime::Runtime>,
    running_local_api_socket_server: Option<crate::local_api_socket::RunningLocalApiSocketServer>,
    runtime_id: Option<String>,
}

impl ManualProcessor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // The lifecycle thread that calls `setup` / `start` is not guaranteed
        // to be inside a tokio runtime, and axum::serve +
        // tokio::net::UnixListener need their reactor / timer thread-local
        // state set by a runtime's own worker threads — so the processor owns
        // and drives its own.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| {
                Error::Runtime(format!("ApiServer: failed to build tokio runtime: {e}"))
            })?;
        let tokio_handle = runtime.handle().clone();
        self.tokio_runtime = Some(runtime);

        // Capture just the narrow handles the HTTP server task needs;
        // the long-lived task never holds a `RuntimeContext`.
        let runtime_id = ctx.runtime_id();
        let local_api_socket_path = ctx
            .runtime_directory()
            .local_api_socket_path(&RuntimeUniqueId::from(runtime_id.as_str()));
        self.handles = Some(StashedHandles {
            runtime: ctx.runtime(),
            tokio_handle,
            runtime_id,
            local_api_socket_path,
        });
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Drop the runtime — shuts down worker threads and waits for any
        // outstanding spawned tasks to finish. `stop()` already signalled
        // the HTTP server to exit, so this is the cleanup step.
        self.tokio_runtime.take();
        self.handles.take();
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let handles = self
            .handles
            .as_ref()
            .expect("setup must be called before start");

        let runtime = handles.runtime.clone();
        self.running_local_api_socket_server =
            Some(crate::local_api_socket::serve_router_on_local_api_socket(
                |local_api_stopping_token| {
                    crate::handlers::build_router(runtime, local_api_stopping_token)
                },
                &handles.tokio_handle,
                &handles.local_api_socket_path,
            )?);
        self.runtime_id = Some(handles.runtime_id.clone());

        tracing::info!(
            "Local API listening on {}",
            handles.local_api_socket_path.display()
        );

        // Publish a discovery entry so a CLI can find this live control plane.
        // The socket is served, so the entry's existence tracks the control
        // endpoint's. A write failure is non-fatal — the node starts
        // regardless; it just won't be discoverable until the next run.
        let entry = crate::node_registry::NodeRegistryEntry::for_current_process(
            handles.runtime_id.clone(),
            ctx.runtime_name(),
            handles.local_api_socket_path.clone(),
        );
        match crate::node_registry::write_entry(
            &ctx.runtime_directory().node_registry_directory(),
            &entry,
        ) {
            Ok(path) => tracing::debug!("Node registry entry written at {}", path.display()),
            Err(error) => {
                tracing::warn!(%error, "failed to write node registry entry; node not discoverable")
            }
        }

        Ok(())
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Tear down the discovery entry alongside the control endpoint it
        // advertises. Non-fatal on failure — a stale entry is pruned by the
        // reader's liveness check.
        if let Some(runtime_id) = self.runtime_id.take()
            && let Err(error) = crate::node_registry::remove_entry(
                &ctx.runtime_directory().node_registry_directory(),
                &runtime_id,
            )
        {
            tracing::warn!(%error, "failed to remove node registry entry on stop");
        }
        drop(self.running_local_api_socket_server.take());
        Ok(())
    }
}
