// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API of the machine's runtime: its socket at a fixed path in the
//! runtime directory, and the router served on the engine's tokio runtime
//! over every stream the engine loads.

use std::sync::Arc;

use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, Runner};
use streamlib_relay_adapter_sdk::RelayInstancesOfThisMachine;

use crate::local_api_socket::{RunningLocalApiSocketServer, bind_local_api_socket};

/// Serve `engine`'s local API, its relay tools driving `relay_instances`, until
/// the returned holder is dropped: bind `<runtime directory>/local-api.sock`
/// (owner-only, a stale file replaced, a live one refused) and serve the router
/// on the engine's tokio runtime.
///
/// A socket that cannot be bound is refused here: a runtime nobody can reach
/// over its local API does not run.
pub fn serve_the_local_api_for_an_engine(
    engine: &Arc<Runner>,
    relay_instances: Arc<dyn RelayInstancesOfThisMachine>,
) -> Result<LocalApiServedForAnEngine> {
    let local_api_socket_path = engine.runtime_directory().local_api_socket_path();
    let operations_on_the_loaded_streams =
        Arc::clone(engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let running_local_api_socket_server = bind_local_api_socket(&local_api_socket_path)?
        .serve_router(
            |local_api_stopping_token| {
                crate::handlers::build_router(
                    operations_on_the_loaded_streams,
                    relay_instances,
                    local_api_stopping_token,
                )
            },
            &engine.tokio_handle(),
        )?;
    tracing::info!("Local API listening on {}", local_api_socket_path.display());

    Ok(LocalApiServedForAnEngine {
        _running_local_api_socket_server: running_local_api_socket_server,
    })
}

/// An engine's local API being served. Dropping it stops serving and removes
/// the socket file.
#[must_use = "dropping this stops serving the engine's local API"]
#[derive(Debug)]
pub struct LocalApiServedForAnEngine {
    _running_local_api_socket_server: RunningLocalApiSocketServer,
}
