// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The boot recipe a host follows to stand this crate's control plane up inside
//! its own runtime — `tatolabd`, and any Rust app that wants a node
//! `tatolab nodes` can find.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use parking_lot::Mutex;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

use crate::local_api_socket::{LocalApiSocketBoundAndNotYetServed, bind_local_api_socket};

/// Each local API socket a host bound before its runtime started, keyed by its
/// path, until the `ApiServer` processor serving it takes it: a processor's
/// config is JSON and cannot carry a listener.
static LOCAL_API_SOCKETS_BOUND_BY_THEIR_HOST: LazyLock<
    Mutex<HashMap<PathBuf, LocalApiSocketBoundAndNotYetServed>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Take the local API socket a host bound at `local_api_socket_path`, refusing
/// when none was.
pub(crate) fn take_the_local_api_socket_its_host_bound(
    local_api_socket_path: &Path,
) -> Result<LocalApiSocketBoundAndNotYetServed> {
    LOCAL_API_SOCKETS_BOUND_BY_THEIR_HOST
        .lock()
        .remove(local_api_socket_path)
        .ok_or_else(|| {
            Error::Runtime(format!(
                "ApiServer: no local API socket was bound at {} before the runtime started; the \
                 local API is hosted only through \
                 register_api_server_control_plane_processor_on_runtime, once per runtime",
                local_api_socket_path.display()
            ))
        })
}

/// How a host binary stands up the control plane it hosts. It carries no
/// setting: the local API socket's path follows from the runtime.
#[derive(Debug, Clone, Default)]
pub struct ApiServerControlPlaneHostConfig {}

/// Bind `runtime`'s local API socket, register the `ApiServer` processor type
/// in-process and add one instance to `runtime`, so that starting the runtime
/// serves the socket and publishes the node-registry entry `tatolab nodes`
/// discovers, under the runtime's own name.
///
/// A socket that cannot be bound refuses here, before the runtime starts: a
/// runtime nobody can reach over its local API does not run.
pub fn register_api_server_control_plane_processor_on_runtime(
    runtime: &Runner,
    ApiServerControlPlaneHostConfig {}: ApiServerControlPlaneHostConfig,
) -> Result<()> {
    let local_api_socket_bound_by_this_host = bind_local_api_socket(
        &runtime
            .runtime_directory()
            .local_api_socket_path(runtime.runtime_id()),
    )?;

    // A host, not a loadable plugin: the type is statically linked into the
    // caller and registered on the shared registry rather than dlopen'd.
    PROCESSOR_REGISTRY.register::<crate::api_server::ApiServerProcessor::Processor>();

    let api_server_config = serde_json::to_value(crate::ApiServerConfig {
        log_path: runtime
            .jsonl_log_path()
            .map(|jsonl_log_path| jsonl_log_path.to_string_lossy().into_owned()),
    })
    .map_err(|encoding_failure| {
        Error::Config(format!(
            "ApiServer: failed to encode its config: {encoding_failure}"
        ))
    })?;

    runtime.add_processor(ProcessorSpec::new(
        crate::api_server::ApiServerProcessor::processor_class_import_path(),
        api_server_config,
    ))?;

    hand_the_bound_local_api_socket_to_its_processor(local_api_socket_bound_by_this_host)
}

/// Leave `local_api_socket_bound_by_this_host` for the `ApiServer` processor
/// serving it to take, refusing a path that already holds one.
fn hand_the_bound_local_api_socket_to_its_processor(
    local_api_socket_bound_by_this_host: LocalApiSocketBoundAndNotYetServed,
) -> Result<()> {
    match LOCAL_API_SOCKETS_BOUND_BY_THEIR_HOST.lock().entry(
        local_api_socket_bound_by_this_host
            .local_api_socket_path()
            .to_path_buf(),
    ) {
        Entry::Occupied(occupied) => Err(Error::Runtime(format!(
            "ApiServer: a local API socket bound at {} is already waiting for its processor",
            occupied.key().display()
        ))),
        Entry::Vacant(vacant) => {
            vacant.insert(local_api_socket_bound_by_this_host);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_socket_no_host_bound_is_refused_naming_its_path() {
        let refusal = take_the_local_api_socket_its_host_bound(Path::new(
            "/nowhere/local-api-Rneverbound.sock",
        ))
        .unwrap_err()
        .to_string();

        assert!(
            refusal.contains("/nowhere/local-api-Rneverbound.sock"),
            "{refusal}"
        );
    }

    #[test]
    fn a_socket_its_host_bound_is_taken_once() {
        let socket_directory = tempfile::tempdir().unwrap();
        let local_api_socket_path = socket_directory.path().join("local-api-Rtakenonce.sock");
        hand_the_bound_local_api_socket_to_its_processor(
            bind_local_api_socket(&local_api_socket_path).unwrap(),
        )
        .unwrap();

        let taken = take_the_local_api_socket_its_host_bound(&local_api_socket_path).unwrap();

        assert_eq!(taken.local_api_socket_path(), local_api_socket_path);
        assert!(take_the_local_api_socket_its_host_bound(&local_api_socket_path).is_err());
    }
}
