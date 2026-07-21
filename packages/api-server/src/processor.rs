// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `ApiServer` processor — owns the HTTP listener lifecycle and binds the
//! shared [`crate::state::AppState`] to per-request handlers.

use std::sync::Arc;

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::runtime::RuntimeOperations;

/// Handles cloned from the setup-time context for use in start().
/// The `tokio_handle` points at this processor's own tokio runtime
/// (constructed in `setup()`) — the processor owns a dedicated runtime
/// rather than assuming the lifecycle thread that calls `setup` / `start`
/// is itself inside one.
struct StashedHandles {
    runtime: Arc<dyn RuntimeOperations>,
    tokio_handle: tokio::runtime::Handle,
    runtime_id: String,
    auth_token: crate::auth::ApiServerBearerToken,
}

/// Docker-style adjectives for runtime name generation.
const ADJECTIVES: &[&str] = &[
    "admiring",
    "brave",
    "clever",
    "dazzling",
    "eager",
    "fancy",
    "graceful",
    "happy",
    "inspiring",
    "jolly",
    "keen",
    "lively",
    "merry",
    "noble",
    "optimistic",
    "peaceful",
    "quirky",
    "radiant",
    "serene",
    "trusting",
    "upbeat",
    "vibrant",
    "witty",
    "xenial",
    "youthful",
    "zealous",
];

/// Docker-style nouns for runtime name generation.
const NOUNS: &[&str] = &[
    "albatross",
    "beaver",
    "cheetah",
    "dolphin",
    "eagle",
    "falcon",
    "gazelle",
    "hawk",
    "ibis",
    "jaguar",
    "koala",
    "leopard",
    "meerkat",
    "nightingale",
    "otter",
    "panther",
    "quail",
    "raven",
    "sparrow",
    "tiger",
    "urchin",
    "viper",
    "walrus",
    "xerus",
    "yak",
    "zebra",
];

/// Generate a Docker-style random name (adjective-noun).
fn generate_runtime_name() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Use time + pid for randomness without adding fastrand dependency
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        ^ (std::process::id() as u64);
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    let hash = hasher.finish();

    let adj = ADJECTIVES[(hash as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((hash >> 32) as usize) % NOUNS.len()];
    format!("{}-{}", adj, noun)
}

#[streamlib::sdk::processor(
    "@tatolab/api-server/ApiServer",
    description = "Runtime API server — HTTP + WebSocket control plane",
    execution = manual,
    config = crate::_generated_::ApiServerConfig,
)]
pub struct ApiServerProcessor {
    handles: Option<StashedHandles>,
    /// Processor-owned tokio runtime. Constructed in `setup()`, dropped in
    /// `teardown()`. axum / hyper / tokio::net all run inside this runtime
    /// — their reactor / timer thread-local state is set when this
    /// runtime's worker threads enter it, so the HTTP server never depends
    /// on the calling lifecycle thread already being inside a tokio runtime.
    tokio_runtime: Option<tokio::runtime::Runtime>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    runtime_id: Option<String>,
    resolved_name: Option<String>,
    actual_port: Option<u16>,
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

        // Auto-generate + persist the bearer token (0600) on first setup;
        // reused across restarts. Every mutating route is gated behind it.
        let auth_token = crate::auth::ApiServerBearerToken::load_or_create_under_data_dir()?;
        tracing::info!(
            "ApiServer bearer token at {}",
            crate::auth::ApiServerBearerToken::default_token_path().display()
        );

        // Capture just the narrow handles the HTTP server task needs;
        // the long-lived task never holds a `RuntimeContext`.
        self.handles = Some(StashedHandles {
            runtime: ctx.runtime(),
            tokio_handle,
            runtime_id: ctx.runtime_id().to_string(),
            auth_token,
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

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let handles = self
            .handles
            .as_ref()
            .expect("setup must be called before start");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        // Resolve runtime name (from config or auto-generate)
        let runtime_name = self
            .config
            .name
            .clone()
            .unwrap_or_else(generate_runtime_name);
        self.resolved_name = Some(runtime_name.clone());

        self.runtime_id = Some(handles.runtime_id.clone());

        let config = self.config.clone();
        let host = config.host.clone();

        // A non-loopback bind exposes the graph-mutation control plane to the
        // network; it stays localhost-only unless explicitly opted into.
        check_bind_allowed(&host, config.allow_remote_bind)?;

        let app = crate::handlers::build_router(
            handles.runtime.clone(),
            handles.auth_token.clone(),
            #[cfg(feature = "moq")]
            handles.runtime_id.clone(),
        );
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

        self.actual_port = Some(actual_port);
        let api_endpoint = format!("{}:{}", host, actual_port);

        tracing::info!("Api server listening on {}", api_endpoint);
        tracing::info!(
            "OpenAPI spec available at http://{}/api/openapi.json",
            api_endpoint
        );

        // Spawn the HTTP server
        tokio_handle.spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.runtime_id.take();
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

/// Whether `host` names a loopback interface. `"localhost"` and any address
/// that parses to a loopback [`std::net::IpAddr`] (`127.0.0.0/8`, `::1`) count;
/// an unspecified address (`0.0.0.0` / `::`), a routable address, or an
/// unresolvable name does not — the conservative default is "not loopback".
fn host_is_loopback(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Reject a non-loopback bind host unless the config opts into a remote bind.
/// The default (`allow_remote_bind` absent / false) keeps the control plane
/// on localhost.
fn check_bind_allowed(host: &str, allow_remote_bind: Option<bool>) -> Result<()> {
    if host_is_loopback(host) || allow_remote_bind == Some(true) {
        return Ok(());
    }
    Err(Error::Configuration(format!(
        "ApiServer: refusing to bind non-loopback host {host:?} — the control plane accepts \
         graph-mutation requests. Set `allow_remote_bind: true` in the ApiServer config to \
         expose it beyond localhost."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_are_recognized() {
        assert!(host_is_loopback("127.0.0.1"));
        assert!(host_is_loopback("127.0.0.5"));
        assert!(host_is_loopback("::1"));
        assert!(host_is_loopback("localhost"));
        assert!(host_is_loopback("LocalHost"));
    }

    #[test]
    fn non_loopback_hosts_are_rejected() {
        assert!(!host_is_loopback("0.0.0.0"));
        assert!(!host_is_loopback("::"));
        assert!(!host_is_loopback("192.168.1.10"));
        assert!(!host_is_loopback("example.com"));
    }

    #[test]
    fn loopback_binds_without_opt_in() {
        assert!(check_bind_allowed("127.0.0.1", None).is_ok());
        assert!(check_bind_allowed("localhost", Some(false)).is_ok());
        assert!(check_bind_allowed("::1", None).is_ok());
    }

    #[test]
    fn non_loopback_bind_requires_explicit_opt_in() {
        // Localhost-only is the default: a wildcard bind is refused unless the
        // config explicitly opts in.
        assert!(check_bind_allowed("0.0.0.0", None).is_err());
        assert!(check_bind_allowed("0.0.0.0", Some(false)).is_err());
        assert!(check_bind_allowed("0.0.0.0", Some(true)).is_ok());
        assert!(check_bind_allowed("192.168.1.10", Some(true)).is_ok());
    }
}
