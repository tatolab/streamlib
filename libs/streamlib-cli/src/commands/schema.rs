// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema management commands.

use anyhow::{bail, Context, Result};
use std::path::Path;
use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::get_runtime_endpoint_request::Query;
use streamlib_broker::proto::{GetRuntimeEndpointRequest, ListRuntimesRequest};
use streamlib_broker::GRPC_PORT;
use streamlib_codegen_shared::parse_processor_yaml_file;

/// List all schemas by querying running runtimes via broker.
pub async fn list(runtime: Option<&str>, url: Option<&str>) -> Result<()> {
    let client = reqwest::Client::new();

    // Collect (runtime_display_name, api_url) pairs
    let runtimes: Vec<(String, String)> = match (runtime, url) {
        (Some(r), _) => {
            let resolved = resolve_runtime_url(r).await?;
            vec![(r.to_string(), resolved)]
        }
        (None, Some(u)) => vec![("direct".to_string(), u.to_string())],
        (None, None) => {
            let endpoint = broker_endpoint();
            let mut broker = BrokerServiceClient::connect(endpoint)
                .await
                .context("Failed to connect to broker. Is the broker running?")?;
            let response = broker
                .list_runtimes(ListRuntimesRequest {})
                .await
                .context("Failed to list runtimes from broker")?
                .into_inner();

            if response.runtimes.is_empty() {
                bail!("No runtimes registered. Start one with 'streamlib run'.");
            }

            response
                .runtimes
                .into_iter()
                .map(|r| {
                    let display = if r.name.is_empty() {
                        r.runtime_id.clone()
                    } else {
                        r.name.clone()
                    };
                    (display, format!("http://{}", r.api_endpoint))
                })
                .collect()
        }
    };

    // Gather schemas from each runtime
    let mut rows: Vec<(String, String, String)> = Vec::new(); // (NAME, TYPE, RUNTIME)

    for (runtime_name, api_url) in &runtimes {
        let schemas_url = format!("{}/api/schemas", api_url);
        let schema_names: Vec<String> = match client.get(&schemas_url).send().await {
            Ok(resp) => resp.json().await.unwrap_or_default(),
            Err(_) => {
                eprintln!("Warning: failed to query schemas from {}", runtime_name);
                continue;
            }
        };

        for name in schema_names {
            let schema_type = if name.contains(".config@") {
                "config"
            } else {
                "data"
            };
            rows.push((name, schema_type.to_string(), runtime_name.clone()));
        }
    }

    rows.sort();
    rows.dedup();

    if rows.is_empty() {
        println!("No schemas found.");
        return Ok(());
    }

    // Compute column widths
    let name_width = rows.iter().map(|r| r.0.len()).max().unwrap_or(4).max(4);
    let type_width = rows.iter().map(|r| r.1.len()).max().unwrap_or(4).max(4);
    let runtime_width = rows.iter().map(|r| r.2.len()).max().unwrap_or(7).max(7);

    // Print kubectl-style table
    println!(
        "{:<name_w$}  {:<type_w$}  {:<rt_w$}",
        "NAME",
        "TYPE",
        "RUNTIME",
        name_w = name_width,
        type_w = type_width,
        rt_w = runtime_width,
    );

    for (name, schema_type, runtime_name) in &rows {
        println!(
            "{:<name_w$}  {:<type_w$}  {:<rt_w$}",
            name,
            schema_type,
            runtime_name,
            name_w = name_width,
            type_w = type_width,
            rt_w = runtime_width,
        );
    }

    Ok(())
}

/// Show the YAML definition of a schema by querying a running runtime.
pub async fn describe(name: &str, runtime: Option<&str>, url: Option<&str>) -> Result<()> {
    let resolved_url = resolve_url(runtime, url).await?;
    let client = reqwest::Client::new();

    let schema_url = format!("{}/api/schemas/{}", resolved_url, name);
    let response = client
        .get(&schema_url)
        .send()
        .await
        .context("Failed to connect to runtime. Is a runtime running?")?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        println!("No definition found for schema '{}'.", name);

        // List available definitions from the runtime
        let list_url = format!("{}/api/schemas", resolved_url);
        let available: Vec<String> = client
            .get(&list_url)
            .send()
            .await
            .context("Failed to connect to runtime.")?
            .json()
            .await
            .unwrap_or_default();

        if !available.is_empty() {
            println!("\nAvailable schema definitions:");
            for s in &available {
                println!("  {}", s);
            }
        }
    } else {
        let definition = response.text().await?;
        println!("{}", definition);
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
