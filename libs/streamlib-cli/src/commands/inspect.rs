// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anyhow::{Context, Result};
use serde::Deserialize;

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

/// Inspect a running runtime by querying its API.
pub async fn run(url: &str) -> Result<()> {
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

/// Get and display the graph from a running runtime.
pub async fn graph(url: &str, format: &str) -> Result<()> {
    let client = reqwest::Client::new();

    let graph_url = format!("{}/api/graph", url);
    let graph: serde_json::Value = client
        .get(&graph_url)
        .send()
        .await
        .context("Failed to connect to runtime")?
        .json()
        .await
        .context("Failed to parse graph response")?;

    match format {
        "json" => {
            println!("{}", serde_json::to_string_pretty(&graph)?);
        }
        "dot" => {
            print_graph_as_dot(&graph)?;
        }
        "pretty" | _ => {
            print_graph_pretty(&graph)?;
        }
    }

    Ok(())
}

fn print_graph_pretty(graph: &serde_json::Value) -> Result<()> {
    let processors = graph
        .get("processors")
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    let links = graph
        .get("links")
        .and_then(|l| l.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    println!("Graph: {} processors, {} links\n", processors, links);

    if let Some(procs) = graph.get("processors").and_then(|p| p.as_array()) {
        println!("Processors:");
        for proc in procs {
            let id = proc.get("id").and_then(|i| i.as_str()).unwrap_or("?");
            let proc_type = proc
                .get("processor_type")
                .and_then(|t| t.as_str())
                .unwrap_or("?");
            println!("  [{}] {}", id, proc_type);
        }
    }

    if let Some(links_arr) = graph.get("links").and_then(|l| l.as_array()) {
        if !links_arr.is_empty() {
            println!("\nLinks:");
            for link in links_arr {
                let from = link.get("from").and_then(|f| f.as_str()).unwrap_or("?");
                let to = link.get("to").and_then(|t| t.as_str()).unwrap_or("?");
                println!("  {} -> {}", from, to);
            }
        }
    }

    Ok(())
}

fn print_graph_as_dot(graph: &serde_json::Value) -> Result<()> {
    println!("digraph streamlib {{");
    println!("  rankdir=LR;");
    println!("  node [shape=box];");

    if let Some(procs) = graph.get("processors").and_then(|p| p.as_array()) {
        for proc in procs {
            let id = proc.get("id").and_then(|i| i.as_str()).unwrap_or("?");
            let proc_type = proc
                .get("processor_type")
                .and_then(|t| t.as_str())
                .unwrap_or("?");
            println!("  \"{}\" [label=\"{}\\n({})\"];", id, id, proc_type);
        }
    }

    if let Some(links) = graph.get("links").and_then(|l| l.as_array()) {
        for link in links {
            let from = link.get("from").and_then(|f| f.as_str()).unwrap_or("?");
            let to = link.get("to").and_then(|t| t.as_str()).unwrap_or("?");
            // Parse "processor.port" format
            let from_proc = from.split('.').next().unwrap_or(from);
            let to_proc = to.split('.').next().unwrap_or(to);
            println!("  \"{}\" -> \"{}\";", from_proc, to_proc);
        }
    }

    println!("}}");

    Ok(())
}
