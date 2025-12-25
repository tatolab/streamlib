// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! API Server Processor Demo
//!
//! Tests all API endpoints of the ApiServerProcessor.

use streamlib::{ApiServerConfig, ApiServerProcessor, Result, StreamRuntime};

const BASE_URL: &str = "http://127.0.0.1:9000";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    println!("=== API Server Processor Demo ===\n");

    let runtime = StreamRuntime::new()?;

    // Add the API server processor
    println!("Adding API server processor...");
    let _api_server = runtime.add_processor(ApiServerProcessor::node(ApiServerConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
    }))?;

    // Start the runtime
    runtime.start()?;

    // Give server a moment to start
    std::thread::sleep(std::time::Duration::from_millis(500));

    println!("\n--- Running API Tests ---\n");

    let client = reqwest::blocking::Client::new();

    // Test 1: Health endpoint
    test_health(&client);

    // Test 2: Get registry
    test_registry(&client);

    // Test 3: Get initial graph
    test_get_graph(&client, "Initial graph");

    // Test 4: Create a processor
    let processor_id = test_create_processor(&client);

    // Test 5: Verify processor in graph
    test_get_graph(&client, "After adding processor");

    // Test 6: Create another processor for connection test
    let processor_id_2 = test_create_processor_2(&client);

    // Test 7: Create connection between processors
    let connection_id = test_create_connection(&client, &processor_id, &processor_id_2);

    // Test 8: Verify connection in graph
    test_get_graph(&client, "After adding connection");

    // Test 9: Delete connection
    test_delete_connection(&client, &connection_id);

    // Test 10: Verify connection removed
    test_get_graph(&client, "After deleting connection");

    // Test 11: Delete processors
    test_delete_processor(&client, &processor_id);
    test_delete_processor(&client, &processor_id_2);

    // Test 12: Verify processors removed
    test_get_graph(&client, "After deleting processors");

    println!("\n--- All Tests Complete ---\n");

    // Shutdown
    runtime.stop()?;

    Ok(())
}

fn test_health(client: &reqwest::blocking::Client) {
    print!("Testing GET /health ... ");
    let resp = client.get(format!("{}/health", BASE_URL)).send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let body = r.text().unwrap_or_default();
            println!("OK ({})", body);
        }
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

fn test_registry(client: &reqwest::blocking::Client) {
    print!("Testing GET /api/registry ... ");
    let resp = client.get(format!("{}/api/registry", BASE_URL)).send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().unwrap_or_default();
            let count = json["processors"].as_array().map(|a| a.len()).unwrap_or(0);
            println!("OK ({} processors registered)", count);
        }
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

fn test_get_graph(client: &reqwest::blocking::Client, label: &str) {
    print!("Testing GET /api/graph ({}) ... ", label);
    let resp = client.get(format!("{}/api/graph", BASE_URL)).send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().unwrap_or_default();
            let nodes = json["nodes"].as_array().map(|a| a.len()).unwrap_or(0);
            let links = json["links"].as_array().map(|a| a.len()).unwrap_or(0);
            println!("OK ({} nodes, {} links)", nodes, links);
            // Debug: print full graph JSON
            println!(
                "  Graph: {}",
                serde_json::to_string_pretty(&json).unwrap_or_default()
            );
        }
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

fn test_create_processor(client: &reqwest::blocking::Client) -> String {
    print!("Testing POST /api/processor (SimplePassthroughProcessor) ... ");
    let body = serde_json::json!({
        "processor_type": "SimplePassthroughProcessor",
        "config": {}
    });
    let resp = client
        .post(format!("{}/api/processor", BASE_URL))
        .json(&body)
        .send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().unwrap_or_default();
            let id = json["id"].as_str().unwrap_or("unknown").to_string();
            println!("OK (id: {})", id);
            id
        }
        Ok(r) => {
            println!("FAIL (status: {})", r.status());
            String::new()
        }
        Err(e) => {
            println!("FAIL ({})", e);
            String::new()
        }
    }
}

fn test_create_processor_2(client: &reqwest::blocking::Client) -> String {
    print!("Testing POST /api/processor (SimplePassthroughProcessor #2) ... ");
    let body = serde_json::json!({
        "processor_type": "SimplePassthroughProcessor",
        "config": {}
    });
    let resp = client
        .post(format!("{}/api/processor", BASE_URL))
        .json(&body)
        .send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().unwrap_or_default();
            let id = json["id"].as_str().unwrap_or("unknown").to_string();
            println!("OK (id: {})", id);
            id
        }
        Ok(r) => {
            println!("FAIL (status: {})", r.status());
            String::new()
        }
        Err(e) => {
            println!("FAIL ({})", e);
            String::new()
        }
    }
}

fn test_create_connection(
    client: &reqwest::blocking::Client,
    from_processor: &str,
    to_processor: &str,
) -> String {
    print!("Testing POST /api/connections ... ");
    let body = serde_json::json!({
        "from_processor": from_processor,
        "from_port": "output",
        "to_processor": to_processor,
        "to_port": "input"
    });
    let resp = client
        .post(format!("{}/api/connections", BASE_URL))
        .json(&body)
        .send();
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().unwrap_or_default();
            let id = json["id"].as_str().unwrap_or("unknown").to_string();
            println!("OK (id: {})", id);
            id
        }
        Ok(r) => {
            let body = r.text().unwrap_or_default();
            println!("FAIL (status: {}, body: {})", body, body);
            String::new()
        }
        Err(e) => {
            println!("FAIL ({})", e);
            String::new()
        }
    }
}

fn test_delete_connection(client: &reqwest::blocking::Client, connection_id: &str) {
    print!("Testing DELETE /api/connections/{} ... ", connection_id);
    let resp = client
        .delete(format!("{}/api/connections/{}", BASE_URL, connection_id))
        .send();
    match resp {
        Ok(r) if r.status().is_success() => println!("OK"),
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

fn test_delete_processor(client: &reqwest::blocking::Client, processor_id: &str) {
    print!("Testing DELETE /api/processors/{} ... ", processor_id);
    let resp = client
        .delete(format!("{}/api/processors/{}", BASE_URL, processor_id))
        .send();
    match resp {
        Ok(r) if r.status().is_success() => println!("OK"),
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}
