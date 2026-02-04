// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema management commands.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;
use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::get_runtime_endpoint_request::Query;
use streamlib_broker::proto::GetRuntimeEndpointRequest;
use streamlib_broker::GRPC_PORT;
use streamlib_codegen_shared::parse_processor_yaml_file;

#[derive(Debug, Deserialize)]
struct RegistryResponse {
    processors: Vec<ProcessorInfo>,
    #[serde(default)]
    schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Deserialize)]
struct ProcessorInfo {
    name: String,
    #[serde(default)]
    inputs: Vec<PortInfo>,
    #[serde(default)]
    outputs: Vec<PortInfo>,
}

#[derive(Debug, Deserialize)]
struct PortInfo {
    #[allow(dead_code)]
    name: String,
    schema: String,
}

#[derive(Debug, Deserialize)]
struct SchemaInfo {
    name: String,
}

/// List all known schemas by querying a running runtime.
pub async fn list(runtime: Option<&str>, url: Option<&str>) -> Result<()> {
    let resolved_url = resolve_url(runtime, url).await?;

    let client = reqwest::Client::new();
    let registry_url = format!("{}/api/registry", resolved_url);
    let registry: RegistryResponse = client
        .get(&registry_url)
        .send()
        .await
        .context("Failed to connect to runtime. Is a runtime running?")?
        .json()
        .await
        .context("Failed to parse registry response")?;

    if registry.schemas.is_empty() {
        println!("No schemas found in the runtime registry.");
        return Ok(());
    }

    // Build a map: schema -> list of (processor_name, direction)
    let mut schema_usage: std::collections::BTreeMap<String, Vec<(String, &str)>> =
        std::collections::BTreeMap::new();

    for processor in &registry.processors {
        for input in &processor.inputs {
            schema_usage
                .entry(input.schema.clone())
                .or_default()
                .push((processor.name.clone(), "input"));
        }
        for output in &processor.outputs {
            schema_usage
                .entry(output.schema.clone())
                .or_default()
                .push((processor.name.clone(), "output"));
        }
    }

    println!("Known schemas ({}):\n", registry.schemas.len());

    for schema in &registry.schemas {
        let has_definition =
            streamlib::core::embedded_schemas::get_embedded_schema_definition(&schema.name)
                .is_some();
        let def_marker = if has_definition { " [definition]" } else { "" };
        println!("  {}{}", schema.name, def_marker);

        if let Some(usages) = schema_usage.get(&schema.name) {
            for (processor, direction) in usages {
                println!("    {} ({})", processor, direction);
            }
        }
        println!();
    }

    Ok(())
}

/// Show the YAML definition of a schema.
pub fn get(name: &str) -> Result<()> {
    match streamlib::core::embedded_schemas::get_embedded_schema_definition(name) {
        Some(definition) => {
            println!("{}", definition);
        }
        None => {
            println!("No definition found for schema '{}'.", name);

            // Show available schemas
            let available = streamlib::core::embedded_schemas::list_embedded_schema_names();
            println!("\nAvailable schema definitions:");
            for s in &available {
                println!("  {}", s);
            }
        }
    }
    Ok(())
}

/// Validate a processor YAML schema file.
pub fn validate_processor(path: &Path) -> Result<()> {
    println!("Validating processor schema: {}", path.display());

    match parse_processor_yaml_file(path) {
        Ok(schema) => {
            println!();
            println!("  Name:        {}", schema.name);
            println!("  Version:     {}", schema.version);
            if let Some(desc) = &schema.description {
                println!("  Description: {}", desc);
            }
            println!("  Runtime:     {:?}", schema.runtime.language);
            if schema.runtime.options.unsafe_send {
                println!("  Options:     unsafe_send=true");
            }
            println!("  Execution:   {:?}", schema.execution);
            if let Some(config) = &schema.config {
                println!("  Config:      {} (schema: {})", config.name, config.schema);
            }
            if !schema.inputs.is_empty() {
                println!("  Inputs:");
                for input in &schema.inputs {
                    println!("    - {} ({})", input.name, input.schema);
                }
            }
            if !schema.outputs.is_empty() {
                println!("  Outputs:");
                for output in &schema.outputs {
                    println!("    - {} ({})", output.name, output.schema);
                }
            }
            println!();
            println!("Processor schema is valid.");
            Ok(())
        }
        Err(e) => {
            println!();
            anyhow::bail!("Validation failed: {}", e);
        }
    }
}

fn broker_endpoint() -> String {
    let port = std::env::var("STREAMLIB_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(GRPC_PORT);
    format!("http://127.0.0.1:{}", port)
}

async fn resolve_runtime_url(runtime: &str) -> Result<String> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker. Is the broker running?")?;

    let request = GetRuntimeEndpointRequest {
        query: Some(Query::Name(runtime.to_string())),
    };

    let response = client
        .get_runtime_endpoint(request)
        .await
        .context("Failed to query broker for runtime endpoint")?
        .into_inner();

    if !response.found {
        bail!(
            "Runtime '{}' not found. Use 'streamlib runtimes list' to see available runtimes.",
            runtime
        );
    }

    Ok(format!("http://{}", response.api_endpoint))
}

async fn resolve_url(runtime: Option<&str>, url: Option<&str>) -> Result<String> {
    match (runtime, url) {
        (Some(r), _) => resolve_runtime_url(r).await,
        (None, Some(u)) => Ok(u.to_string()),
        (None, None) => Ok("http://127.0.0.1:9000".to_string()),
    }
}
