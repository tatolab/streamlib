// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `ApiServer` processor — owns the HTTP listener lifecycle and binds the
//! shared [`crate::state::AppState`] to per-request handlers.

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
    /// `Some` only when the config opted into bearer auth; `None` leaves the
    /// shutdown route and the tap WebSocket open (the zero-ceremony default).
    auth_token: Option<crate::auth::ApiServerBearerToken>,
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
    /// Signals both listeners — the TCP port and the local API socket — to stop serving.
    shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    runtime_id: Option<String>,
    actual_port: Option<u16>,
    /// The local API socket this processor bound, removed again at stop.
    bound_local_api_socket_path: Option<PathBuf>,
}

impl ManualProcessor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Construct this processor's own tokio runtime. The lifecycle
        // thread that calls `setup` / `start` is not guaranteed to be
        // inside a tokio runtime, and axum::serve + tokio::net::TcpListener
        // need their reactor / timer thread-local state set by a runtime's
        // own worker threads — so the processor owns and drives its own.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| {
                Error::Runtime(format!("ApiServer: failed to build tokio runtime: {e}"))
            })?;
        let tokio_handle = runtime.handle().clone();
        self.tokio_runtime = Some(runtime);

        // Bearer auth is opt-in (default off): a node runs locally with full
        // permission, so the gated routes stay open unless the config asks for
        // a token. When enabled, auto-generate + 0600-persist the secret on
        // first setup (reused across restarts) and gate `POST
        // /api/runtime/shutdown`, the tap WebSocket, and `POST /mcp`.
        let auth_token = if self.config.require_auth == Some(true) {
            let token = crate::auth::ApiServerBearerToken::load_or_create_under_data_dir()?;
            tracing::info!(
                "ApiServer bearer token at {}",
                crate::auth::ApiServerBearerToken::default_token_path().display()
            );
            Some(token)
        } else {
            None
        };

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
            auth_token,
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
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let config = self.config.clone();
        let host = config.host.clone();

        let app =
            crate::handlers::build_router(handles.runtime.clone(), handles.auth_token.clone());
        let base_port = config.port;
        let tokio_handle = handles.tokio_handle.clone();

        // Try to bind to port, incrementing if in use (up to 10 attempts)
        let (listener, actual_port) = tokio_handle.block_on(async {
            for port_offset in 0..10u16 {
                let port = base_port + port_offset;
                let addr = format!("{}:{}", host, port);
                match tokio::net::TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        if port_offset > 0 {
                            tracing::info!("Port {} in use, bound to {} instead", base_port, port);
                        }
                        return Ok((listener, port));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        continue;
                    }
                    Err(e) => {
                        return Err(Error::Other(anyhow::anyhow!(
                            "Failed to bind to {}: {}",
                            addr,
                            e
                        )));
                    }
                }
            }
            Err(Error::Other(anyhow::anyhow!(
                "Could not find available port in range {}-{}",
                base_port,
                base_port + 9
            )))
        })?;

        let local_api_listener = {
            let _entered_tokio_runtime = tokio_handle.enter();
            crate::local_api_socket::bind_local_api_unix_listener(&handles.local_api_socket_path)?
        };
        self.bound_local_api_socket_path = Some(handles.local_api_socket_path.clone());
        self.shutdown_tx = Some(shutdown_tx);
        self.runtime_id = Some(handles.runtime_id.clone());

        self.actual_port = Some(actual_port);
        let api_endpoint = format!("{}:{}", host, actual_port);

        tracing::info!("Api server listening on {}", api_endpoint);
        tracing::info!(
            "Local API listening on {}",
            handles.local_api_socket_path.display()
        );

        // Publish a discovery entry so a CLI can find this live control plane.
        // The endpoint exists now (the port is bound), so the entry's existence
        // tracks the control endpoint's. A write failure is non-fatal — the node
        // starts regardless; it just won't be discoverable until the next run.
        let control_url = format!("http://127.0.0.1:{}", actual_port);
        let entry = crate::node_registry::NodeRegistryEntry::for_current_process(
            handles.runtime_id.clone(),
            ctx.runtime_name(),
            control_url,
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

        tracing::info!(
            "OpenAPI spec available at http://{}/api/openapi.json",
            api_endpoint
        );

        // The socket is served with no bearer gate: its file mode is the whole gate.
        let local_api_app = crate::handlers::build_router(handles.runtime.clone(), None);
        tokio_handle.spawn(serve_until_shutdown(
            listener,
            app,
            shutdown_rx.clone(),
            "control-plane TCP port",
        ));
        tokio_handle.spawn(serve_until_shutdown(
            local_api_listener,
            local_api_app,
            shutdown_rx,
            "local API socket",
        ));

        Ok(())
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Tear down the discovery entry alongside the control endpoint it
        // advertises. Non-fatal on failure — a stale entry is pruned by the
        // reader's liveness check.
        if let Some(runtime_id) = self.runtime_id.take() {
            if let Err(error) = crate::node_registry::remove_entry(
                &ctx.runtime_directory().node_registry_directory(),
                &runtime_id,
            ) {
                tracing::warn!(%error, "failed to remove node registry entry on stop");
            }
        }
        if let Some(shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.send(true);
        }
        if let Some(local_api_socket_path) = self.bound_local_api_socket_path.take()
            && let Err(error) =
                crate::local_api_socket::remove_local_api_socket_file(&local_api_socket_path)
        {
            tracing::warn!(
                %error,
                "failed to remove the local API socket {} on stop",
                local_api_socket_path.display()
            );
        }
        Ok(())
    }
}

/// Serve `app` on `listener` until `shutdown_rx` sees the stop signal or its sender drops.
async fn serve_until_shutdown<L>(
    listener: L,
    app: axum::Router,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    listener_description: &'static str,
) where
    L: axum::serve::Listener,
    L::Addr: std::fmt::Debug,
{
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.wait_for(|stop_requested| *stop_requested).await;
        })
        .await;
    if let Err(error) = served {
        tracing::error!(%error, "the {listener_description} stopped serving");
    }
}
