// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API of the machine's runtime: its socket at a fixed path in the
//! runtime directory, and the router served on the engine's tokio runtime
//! over every stream the engine loads.

use std::sync::Arc;
use std::time::Duration;

use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, Runner};

use crate::local_api_socket::{RunningLocalApiSocketServer, bind_local_api_socket};

/// How long letting go of the local API waits for its router — which holds
/// the engine — to be dropped, so the engine is never dropped last on one of
/// its own tokio workers.
const LOCAL_API_ROUTER_DROP_BUDGET: Duration = Duration::from_secs(5);

/// Serve `engine`'s local API until the returned holder is dropped: bind
/// `<runtime directory>/local-api.sock` (owner-only, a stale file replaced, a
/// live one refused) and serve the router on the engine's tokio runtime.
///
/// A socket that cannot be bound is refused here: a runtime nobody can reach
/// over its local API does not run.
pub fn serve_the_local_api_for_an_engine(
    engine: &Arc<Runner>,
) -> Result<LocalApiServedForAnEngine> {
    let local_api_socket_path = engine.runtime_directory().local_api_socket_path();
    let operations_on_the_loaded_streams =
        Arc::clone(engine) as Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>;
    let running_local_api_socket_server = bind_local_api_socket(&local_api_socket_path)?
        .serve_router(
            |local_api_stopping_token| {
                crate::handlers::build_router(
                    operations_on_the_loaded_streams,
                    local_api_stopping_token,
                )
            },
            &engine.tokio_handle(),
        )?;
    tracing::info!("Local API listening on {}", local_api_socket_path.display());

    Ok(LocalApiServedForAnEngine {
        running_local_api_socket_server: Some(running_local_api_socket_server),
    })
}

/// An engine's local API being served. Dropping it stops serving and removes
/// the socket file.
#[must_use = "dropping this stops serving the engine's local API"]
#[derive(Debug)]
pub struct LocalApiServedForAnEngine {
    running_local_api_socket_server: Option<RunningLocalApiSocketServer>,
}

impl Drop for LocalApiServedForAnEngine {
    fn drop(&mut self) {
        let Some(running_local_api_socket_server) = self.running_local_api_socket_server.take()
        else {
            return;
        };
        // Inside a tokio runtime the serving task may need this very worker to
        // end, so the wait is only for a host letting go from its own thread.
        if tokio::runtime::Handle::try_current().is_ok() {
            return;
        }
        if !running_local_api_socket_server
            .stop_serving_and_wait_until_the_router_is_dropped(LOCAL_API_ROUTER_DROP_BUDGET)
        {
            tracing::warn!(
                "the local API's router was still held {LOCAL_API_ROUTER_DROP_BUDGET:?} after it \
                 stopped serving; a connection outlived the shutdown"
            );
        }
    }
}
