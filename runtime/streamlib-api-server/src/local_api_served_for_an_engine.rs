// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The local API of one engine: its socket in the runtime directory, the
//! router served on the engine's tokio runtime over every stream the engine
//! loads, and the node-registry entry `tatolab nodes` discovers it by.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, Runner};
use streamlib_runtime_client_contract::node_registry::{
    NodeRegistryEntry, remove_entry, write_entry,
};

use crate::local_api_socket::{RunningLocalApiSocketServer, bind_local_api_socket};

/// How long letting go of the local API waits for its router — which holds
/// the engine — to be dropped, so the engine is never dropped last on one of
/// its own tokio workers.
const LOCAL_API_ROUTER_DROP_BUDGET: Duration = Duration::from_secs(5);

/// Serve `engine`'s local API until the returned holder is dropped: bind
/// `<runtime directory>/local-api-<runtime_id>.sock` (owner-only, a stale file
/// replaced, a live one refused), serve the router on the engine's tokio
/// runtime, and write the node-registry entry.
///
/// A socket that cannot be bound is refused here: a runtime nobody can reach
/// over its local API does not run.
pub fn serve_the_local_api_for_an_engine(
    engine: &Arc<Runner>,
) -> Result<LocalApiServedForAnEngine> {
    let runtime_id = engine.runtime_id().to_string();
    let local_api_socket_path = engine
        .runtime_directory()
        .local_api_socket_path_for_runtime_id(runtime_id.as_str());
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

    // Written once the socket is served, so the entry's existence tracks the
    // endpoint's. A write failure leaves the runtime running, undiscoverable.
    let node_registry_directory = engine.runtime_directory().node_registry_directory();
    let entry = NodeRegistryEntry::for_current_process(
        runtime_id.clone(),
        engine.runtime_name().as_str(),
        local_api_socket_path,
    );
    match write_entry(&node_registry_directory, &entry) {
        Ok(entry_path) => {
            tracing::debug!("Node registry entry written at {}", entry_path.display())
        }
        Err(error) => {
            tracing::warn!(%error, "failed to write node registry entry; node not discoverable")
        }
    }

    Ok(LocalApiServedForAnEngine {
        running_local_api_socket_server: Some(running_local_api_socket_server),
        node_registry_directory,
        runtime_id,
    })
}

/// An engine's local API being served. Dropping it removes the node-registry
/// entry, stops serving and removes the socket file.
#[must_use = "dropping this stops serving the engine's local API"]
#[derive(Debug)]
pub struct LocalApiServedForAnEngine {
    running_local_api_socket_server: Option<RunningLocalApiSocketServer>,
    node_registry_directory: PathBuf,
    runtime_id: String,
}

impl Drop for LocalApiServedForAnEngine {
    fn drop(&mut self) {
        // A stale entry is pruned by the reader's liveness check.
        if let Err(error) = remove_entry(&self.node_registry_directory, &self.runtime_id) {
            tracing::warn!(%error, "failed to remove the node registry entry");
        }
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
