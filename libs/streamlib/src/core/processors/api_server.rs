// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{InputLinkPortRef, OutputLinkPortRef};
use crate::PROCESSOR_REGISTRY;
use crate::{
    core::{Result, RuntimeContext},
    ProcessorSpec,
};
use axum::{extract::Path, extract::State, Json};
use serde::{Deserialize, Serialize};
use std::future::Future;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9000,
        }
    }
}

#[derive(Clone)]
struct AppState {
    runtime_ctx: RuntimeContext,
}

#[derive(Deserialize)]
struct CreateProcessorRequest {
    processor_type: String,
    config: serde_json::Value,
}

#[derive(Deserialize)]
struct CreateConnectionRequest {
    from_processor: String,
    from_port: String,
    to_processor: String,
    to_port: String,
}

// ------------
#[crate::processor(
    execution = Manual,
    description = "Runtime api server for streamlib"
)]
pub struct ApiServerProcessor {
    #[crate::config]
    config: ApiServerConfig,
    runtime_ctx: Option<RuntimeContext>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        self.runtime_ctx = Some(ctx);
        std::future::ready(Ok(()))
    }

    /// Called once when the processor stops.
    fn teardown(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is paused.
    fn on_pause(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called when the processor is resumed after being paused.
    fn on_resume(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    /// Called once to start the processor.
    fn start(&mut self) -> Result<()> {
        use axum::{routing::delete, routing::get, routing::post, Router};

        let ctx = self
            .runtime_ctx
            .clone()
            .expect("setup must be called before start");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        let state = AppState {
            runtime_ctx: ctx.clone(),
        };

        let app = Router::new()
            .route("/health", get(health))
            .route("/api/graph", get(get_graph))
            .route("/api/processor", post(create_processor))
            .route("/api/processors/{id}", delete(delete_processor))
            .route("/api/connections", post(create_connection))
            .route("/api/connections/{id}", delete(delete_connection))
            .route("/api/registry", get(get_registry))
            .with_state(state);

        let config = self.config.clone();
        let addr = format!("{}:{}", config.host, config.port);

        ctx.tokio_handle().spawn(async move {
            let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
            tracing::info!("Api server listening on {}", addr);
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        Ok(())
    }

    /// Called when the processor should stop.
    ///
    /// This is called before teardown when the runtime shuts down or the processor is removed.
    /// Use this to stop internal threads, callbacks, or processing loops started by `start()`.
    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

async fn health() -> &'static str {
    tracing::info!("Health function called");
    "ok"
}

async fn get_graph(
    State(state): State<AppState>,
) -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    state
        .runtime_ctx
        .runtime()
        .to_json_async()
        .await
        .map(Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn create_processor(
    State(state): State<AppState>,
    Json(body): Json<CreateProcessorRequest>,
) -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let spec = ProcessorSpec::new(&body.processor_type, body.config);

    state
        .runtime_ctx
        .runtime()
        .add_processor_async(spec)
        .await
        .map(|id| Json(serde_json::json!({"id": id.to_string()})))
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
}

async fn delete_processor(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<axum::http::StatusCode, axum::http::StatusCode> {
    let processor_id = id.into();
    state
        .runtime_ctx
        .runtime()
        .remove_processor_async(processor_id)
        .await
        .map(|_| axum::http::StatusCode::NO_CONTENT)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

async fn create_connection(
    State(state): State<AppState>,
    Json(body): Json<CreateConnectionRequest>,
) -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let from = OutputLinkPortRef::new(body.from_processor, body.from_port);
    let to = InputLinkPortRef::new(body.to_processor, body.to_port);

    state
        .runtime_ctx
        .runtime()
        .connect_async(from, to)
        .await
        .map(|id| Json(serde_json::json!({"id": id.to_string()})))
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
}

async fn delete_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<axum::http::StatusCode, axum::http::StatusCode> {
    let link_id = id.into();

    state
        .runtime_ctx
        .runtime()
        .disconnect_async(link_id)
        .await
        .map(|_| axum::http::StatusCode::NO_CONTENT)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

async fn get_registry() -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let processors = PROCESSOR_REGISTRY.list_registered();
    Ok(Json(serde_json::json!({ "processors": processors })))
}
