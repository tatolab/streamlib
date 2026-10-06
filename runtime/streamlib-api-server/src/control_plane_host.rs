// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The boot recipe a host follows to stand this crate's control plane up inside
//! its own runtime — the wheel's `Runtime.host_control_plane` and any Rust app
//! that wants a node `streamlib nodes` can find.

use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

/// Register the `ApiServer` processor type in-process and add one instance to
/// `runtime`, so that starting the runtime binds its local API socket and
/// publishes the node-registry entry `streamlib nodes` discovers, under the
/// runtime's own name.
pub fn register_api_server_control_plane_processor_on_runtime(runtime: &Runner) -> Result<()> {
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

    Ok(())
}
