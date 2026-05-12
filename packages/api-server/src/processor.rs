// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `ApiServer` processor — owns the HTTP listener lifecycle and binds the
//! shared [`crate::state::AppState`] to per-request handlers.

use std::future::Future;
use std::sync::Arc;

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::runtime::RuntimeOperations;

/// Handles cloned from the setup-time context for use in start().
struct StashedHandles {
    runtime: Arc<dyn RuntimeOperations>,
    tokio_handle: tokio::runtime::Handle,
    runtime_id: String,
    #[cfg(feature = "moq")]
    moq_sessions: streamlib::sdk::streaming::SharedMoqSessions,
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

#[streamlib::sdk::processor("ApiServer")]
pub struct ApiServerProcessor {
    handles: Option<StashedHandles>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    runtime_id: Option<String>,
    resolved_name: Option<String>,
    actual_port: Option<u16>,
}

impl ManualProcessor for ApiServerProcessor::Processor {
    fn setup(
        &mut self,
        ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        // Capture just the narrow handles the HTTP server task needs;
        // the long-lived task never holds a `RuntimeContext`.
        self.handles = Some(StashedHandles {
            runtime: ctx.runtime(),
            tokio_handle: ctx.tokio_handle().clone(),
            runtime_id: ctx.runtime_id().to_string(),
            #[cfg(feature = "moq")]
            moq_sessions: ctx.moq_sessions().clone(),
        });
        std::future::ready(Ok(()))
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn on_pause(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn on_resume(
        &mut self,
        _ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
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

        let app = crate::handlers::build_router(
            handles.runtime.clone(),
            #[cfg(feature = "moq")]
            handles.moq_sessions.clone(),
        );

        let config = self.config.clone();
        let host = config.host.clone();
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
