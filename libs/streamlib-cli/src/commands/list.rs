// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::get_runtime_endpoint_request::Query;
use streamlib_broker::proto::GetRuntimeEndpointRequest;
use streamlib_broker::GRPC_PORT;

#[derive(Debug, Deserialize)]
struct RegistryResponse {
    processors: Vec<ProcessorInfo>,
}

#[derive(Debug, Deserialize)]
struct ProcessorInfo {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    inputs: Vec<PortInfo>,
    #[serde(default)]
    outputs: Vec<PortInfo>,
}

#[derive(Debug, Deserialize)]
struct PortInfo {
    name: String,
    schema: String,
}

/// List all registered processor types by querying a running runtime.
pub async fn processors(runtime: Option<&str>, url: Option<&str>) -> Result<()> {
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

    if registry.processors.is_empty() {
        println!("No processors registered.");
        return Ok(());
    }

    println!("Available processors ({}):\n", registry.processors.len());

    for processor in &registry.processors {
        println!("  {}", processor.name);
        if !processor.description.is_empty() {
            println!("    {}", processor.description);
        }

        if !processor.inputs.is_empty() {
            println!("    Inputs:");
            for input in &processor.inputs {
                println!("      - {} ({})", input.name, input.schema);
            }
        }

        if !processor.outputs.is_empty() {
            println!("    Outputs:");
            for output in &processor.outputs {
                println!("      - {} ({})", output.name, output.schema);
            }
        }

        println!();
    }

    Ok(())
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
