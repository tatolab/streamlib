// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::get_runtime_endpoint_request::Query;
use streamlib_broker::proto::{GetRuntimeEndpointRequest, ListRuntimesRequest};
use streamlib_broker::GRPC_PORT;

#[derive(Debug, Deserialize)]
struct RegistryResponse {
    processors: Vec<ProcessorInfo>,
    schemas: Vec<SchemaInfo>,
}

#[derive(Debug, Deserialize)]
struct ProcessorInfo {
    name: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct SchemaInfo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct GraphResponse {
    nodes: Vec<GraphNode>,
    links: Vec<GraphLink>,
}

#[derive(Debug, Deserialize)]
struct GraphNode {
    id: String,
    #[serde(rename = "type")]
    processor_type: String,
}

#[derive(Debug, Deserialize)]
struct GraphLink {
    source: GraphPortRef,
    target: GraphPortRef,
}

#[derive(Debug, Deserialize)]
struct GraphPortRef {
    processor_id: String,
    port_name: String,
}

/// Inspect a running runtime by querying its API.
pub async fn run(runtime: Option<&str>, url: Option<&str>) -> Result<()> {
    let url = resolve_url(runtime, url).await?;
    let client = reqwest::Client::new();

    // Check health first
    let health_url = format!("{}/health", url);
    let health = client
        .get(&health_url)
        .send()
        .await
        .context("Failed to connect to runtime")?
        .text()
        .await?;

    if health != "ok" {
        anyhow::bail!("Runtime health check failed: {}", health);
    }

    println!("Runtime at {} is healthy\n", url);

    // Get registry
    let registry_url = format!("{}/api/registry", url);
    let registry: RegistryResponse = client
        .get(&registry_url)
        .send()
        .await
        .context("Failed to get registry")?
        .json()
        .await
        .context("Failed to parse registry response")?;

    println!("Registered processors ({}):", registry.processors.len());
    for proc in &registry.processors {
        println!("  - {}", proc.name);
        if !proc.description.is_empty() {
            println!("    {}", proc.description);
        }
    }

    println!("\nRegistered schemas ({}):", registry.schemas.len());
    for schema in &registry.schemas {
        println!("  - {}", schema.name);
    }

    Ok(())
}

/// Get the broker gRPC endpoint.
fn broker_endpoint() -> String {
    let port = std::env::var("STREAMLIB_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(GRPC_PORT);
    format!("http://127.0.0.1:{}", port)
}

/// Resolve runtime name/ID to API URL via broker.
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

/// Resolve runtime URL via broker. Auto-selects if exactly one runtime is registered.
async fn resolve_url(runtime: Option<&str>, url: Option<&str>) -> Result<String> {
    match (runtime, url) {
        (Some(r), _) => resolve_runtime_url(r).await,
        (None, Some(u)) => Ok(u.to_string()),
        (None, None) => {
            let endpoint = broker_endpoint();
            let mut client = BrokerServiceClient::connect(endpoint)
                .await
                .context("Failed to connect to broker. Is the broker running?")?;

            let response = client
                .list_runtimes(ListRuntimesRequest {})
                .await
                .context("Failed to list runtimes from broker")?
                .into_inner();

            match response.runtimes.len() {
                0 => bail!("No runtimes registered. Start one with 'streamlib run'."),
                1 => Ok(format!("http://{}", response.runtimes[0].api_endpoint)),
                n => {
                    let names: Vec<&str> = response
                        .runtimes
                        .iter()
                        .map(|r| {
                            if r.name.is_empty() {
                                r.runtime_id.as_str()
                            } else {
                                r.name.as_str()
                            }
                        })
                        .collect();
                    bail!(
                        "{} runtimes registered. Specify one with --runtime:\n  {}",
                        n,
                        names.join("\n  ")
                    )
                }
            }
        }
    }
}

/// Get and display the graph from a running runtime.
pub async fn graph(runtime: Option<&str>, url: Option<&str>, format: &str) -> Result<()> {
    let resolved_url = resolve_url(runtime, url).await?;

    let client = reqwest::Client::new();

    let graph_url = format!("{}/api/graph", resolved_url);

    match format {
        "json" => {
            // Echo the full JSON response verbatim
            let raw: serde_json::Value = client
                .get(&graph_url)
                .send()
                .await
                .context("Failed to connect to runtime")?
                .json()
                .await
                .context("Failed to parse graph response")?;
            println!("{}", serde_json::to_string_pretty(&raw)?);
        }
        _ => {
            let graph: GraphResponse = client
                .get(&graph_url)
                .send()
                .await
                .context("Failed to connect to runtime")?
                .json()
                .await
                .context("Failed to parse graph response")?;

            if format == "dot" {
                print_graph_as_dot(&graph);
            } else {
                print_graph_pretty(&graph);
            }
        }
    }

    Ok(())
}

fn print_graph_pretty(graph: &GraphResponse) {
    println!(
        "Graph: {} processors, {} links\n",
        graph.nodes.len(),
        graph.links.len()
    );

    if !graph.nodes.is_empty() {
        println!("Processors:");
        for node in &graph.nodes {
            println!("  [{}] {}", node.id, node.processor_type);
        }
    }

    if !graph.links.is_empty() {
        println!("\nLinks:");
        for link in &graph.links {
            println!(
                "  {}.{} -> {}.{}",
                link.source.processor_id,
                link.source.port_name,
                link.target.processor_id,
                link.target.port_name
            );
        }
    }
}

fn print_graph_as_dot(graph: &GraphResponse) {
    println!("digraph streamlib {{");
    println!("  rankdir=LR;");
    println!("  node [shape=box];");

    for node in &graph.nodes {
        println!(
            "  \"{}\" [label=\"{}\\n({})\"];",
            node.id, node.id, node.processor_type
        );
    }

    for link in &graph.links {
        println!(
            "  \"{}\" -> \"{}\";",
            link.source.processor_id, link.target.processor_id
        );
    }

    println!("}}");
}
