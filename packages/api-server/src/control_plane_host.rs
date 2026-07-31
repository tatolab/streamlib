// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The boot recipe a host binary follows to stand this crate's control plane up
//! inside its own runtime — shared by the `streamlib-runtime` binary and the
//! CLI's `run` / `dev` verbs so a node is discoverable the same way whichever
//! one launched it.

use streamlib::sdk::error::Result;
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::{PROCESSOR_REGISTRY, ProcessorSpec};
use streamlib::sdk::runtime::Runner;

/// Where a host binary binds the control plane it hosts.
pub struct ApiServerControlPlaneBindConfig {
    /// Address the HTTP listener binds.
    pub bind_host: String,
    /// Requested port; the api-server increments on collision.
    pub bind_port: u16,
    /// Node name published to the registry; the api-server generates a
    /// Docker-style one when this is `None`.
    pub node_name: Option<String>,
}

/// Register the `ApiServer` processor type in-process and add one instance to
/// `runtime`, so that starting the runtime binds a control endpoint and
/// publishes the node-registry entry `streamlib nodes` discovers.
pub fn register_api_server_control_plane_processor_on_runtime(
    runtime: &Runner,
    config: ApiServerControlPlaneBindConfig,
) -> Result<()> {
    // A host, not a loadable plugin: the type is statically linked into the
    // caller and registered on the shared registry rather than dlopen'd.
    PROCESSOR_REGISTRY.register::<crate::api_server::ApiServerProcessor::Processor>();

    let mut api_server_config = serde_json::Map::new();
    api_server_config.insert("host".into(), serde_json::Value::from(config.bind_host));
    api_server_config.insert("port".into(), serde_json::Value::from(config.bind_port));
    if let Some(node_name) = config.node_name {
        api_server_config.insert("name".into(), serde_json::Value::from(node_name));
    }
    if let Some(jsonl_log_path) = runtime.jsonl_log_path() {
        api_server_config.insert(
            "log_path".into(),
            serde_json::Value::from(jsonl_log_path.to_string_lossy().into_owned()),
        );
    }

    runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "api-server", "ApiServer"),
        serde_json::Value::Object(api_server_config),
    ))?;

    Ok(())
}
