// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API of the machine's runtime: its socket at a fixed path in the
//! runtime directory, and the router served on the engine's tokio runtime
//! over every stream the engine loads.

use std::sync::Arc;

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, Runner};

use crate::local_api_socket::{RunningLocalApiSocketServer, bind_local_api_socket};
use crate::loopback_http_listener::ControlWebSocketRouteForTheLoopbackListener;

/// Serve `engine`'s local API until the returned holder is dropped: bind `<runtime directory>/local-api.sock`
/// (owner-only, a stale file replaced, a live one refused) and serve the router
/// on the engine's tokio runtime.
///
/// A socket that cannot be bound is refused here: a runtime nobody can reach
/// over its local API does not run.
pub fn serve_the_local_api_for_an_engine(
    engine: &Arc<Runner>,
) -> Result<LocalApiServedForAnEngine> {
    let local_api_socket_path = engine.runtime_directory().local_api_socket_path();
    let operations_on_the_loaded_streams =
        Arc::clone(engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let mut control_websocket_route_for_the_loopback_listener = None;
    let running_local_api_socket_server = bind_local_api_socket(&local_api_socket_path)?
        .serve_router(
            |local_api_stopping_token| {
                let local_api_routers = crate::handlers::build_router(
                    operations_on_the_loaded_streams,
                    local_api_stopping_token,
                );
                control_websocket_route_for_the_loopback_listener =
                    Some(local_api_routers.control_websocket_route_for_the_loopback_listener);
                local_api_routers.local_api_socket_router
            },
            &engine.tokio_handle(),
        )?;
    tracing::info!("Local API listening on {}", local_api_socket_path.display());

    Ok(LocalApiServedForAnEngine {
        control_websocket_route_for_the_loopback_listener:
            control_websocket_route_for_the_loopback_listener.ok_or_else(|| {
                Error::Runtime(
                    "the local API was served without building its control WebSocket".to_owned(),
                )
            })?,
        _running_local_api_socket_server: running_local_api_socket_server,
    })
}

/// An engine's local API being served. Dropping it stops serving and removes
/// the socket file.
#[must_use = "dropping this stops serving the engine's local API"]
#[derive(Debug)]
pub struct LocalApiServedForAnEngine {
    control_websocket_route_for_the_loopback_listener: ControlWebSocketRouteForTheLoopbackListener,
    _running_local_api_socket_server: RunningLocalApiSocketServer,
}

impl LocalApiServedForAnEngine {
    /// This local API's control WebSocket, for the loopback listener to serve; it ends each
    /// connection when this local API stops.
    pub fn control_websocket_route_for_the_loopback_listener(
        &self,
    ) -> ControlWebSocketRouteForTheLoopbackListener {
        self.control_websocket_route_for_the_loopback_listener
            .clone()
    }
}
