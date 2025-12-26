// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! API Server Processor Demo
//!
//! Tests all API endpoints of the ApiServerProcessor, including WebSocket event streaming.

use std::sync::{Arc, Mutex};
use std::thread;
use streamlib::{ApiServerConfig, ApiServerProcessor, Result, StreamRuntime};
use tungstenite::{connect, stream::MaybeTlsStream, Message};

const BASE_URL: &str = "http://127.0.0.1:9000";
const WS_URL: &str = "ws://127.0.0.1:9000/ws/events";

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

    // Start WebSocket event collector
    let events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let ws_events = Arc::clone(&events);
    let ws_stop = Arc::new(Mutex::new(false));
    let ws_stop_flag = Arc::clone(&ws_stop);

    let ws_handle = thread::spawn(move || {
        collect_websocket_events(ws_events, ws_stop_flag);
    });

    // Give WebSocket a moment to connect
    std::thread::sleep(std::time::Duration::from_millis(200));

    println!("\n--- Running REST API Tests ---\n");

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

    println!("\n--- REST API Tests Complete ---\n");

    // Give events time to be delivered
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Stop WebSocket collector
    *ws_stop.lock().unwrap() = true;
    let _ = ws_handle.join();

    // Verify WebSocket events
    println!("--- Verifying WebSocket Events ---\n");
    verify_websocket_events(&events);

    println!("\n--- All Tests Complete ---\n");

    // Shutdown
    runtime.stop()?;

    Ok(())
}

/// Collect WebSocket events in background
fn collect_websocket_events(events: Arc<Mutex<Vec<serde_json::Value>>>, stop: Arc<Mutex<bool>>) {
    let (mut socket, _response) = match connect(WS_URL) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("WebSocket connection error: {}", e);
            return;
        }
    };

    println!("WebSocket connected to {}", WS_URL);

    // Set socket to non-blocking for polling
    let stream = socket.get_mut();
    if let MaybeTlsStream::Plain(tcp) = stream {
        if let Err(e) = tcp.set_nonblocking(true) {
            eprintln!("Failed to set non-blocking: {}", e);
            return;
        }
    }

    loop {
        // Check stop flag
        if *stop.lock().unwrap() {
            break;
        }

        match socket.read() {
            Ok(Message::Text(text)) => {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                    events.lock().unwrap().push(json);
                }
            }
            Ok(Message::Close(_)) => break,
            Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No data available, sleep briefly and retry
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                // Connection closed or error
                if !*stop.lock().unwrap() {
                    eprintln!("WebSocket read error: {}", e);
                }
                break;
            }
            _ => {}
        }
    }

    let _ = socket.close(None);
}

/// Verify that expected events were received via WebSocket
fn verify_websocket_events(events: &Arc<Mutex<Vec<serde_json::Value>>>) {
    let events = events.lock().unwrap();
    println!("Received {} WebSocket events", events.len());

    // Count event types
    let mut processor_add_count = 0;
    let mut processor_remove_count = 0;
    let mut link_wire_count = 0;
    let mut link_unwire_count = 0;
    let mut graph_change_count = 0;
    let mut compiler_count = 0;

    for event in events.iter() {
        if let Some(runtime_event) = event.get("RuntimeGlobal") {
            // Check for specific event types
            if runtime_event.get("RuntimeDidAddProcessor").is_some() {
                processor_add_count += 1;
            } else if runtime_event.get("RuntimeDidRemoveProcessor").is_some() {
                processor_remove_count += 1;
            } else if runtime_event.get("CompilerDidWireLink").is_some() {
                link_wire_count += 1;
            } else if runtime_event.get("CompilerDidUnwireLink").is_some() {
                link_unwire_count += 1;
            } else if runtime_event.get("GraphDidChange").is_some() {
                graph_change_count += 1;
            } else if runtime_event.get("CompilerDidCompile").is_some() {
                compiler_count += 1;
            }
        }
    }

    // Report results
    println!("Event Summary:");
    println!("  - Processor added events:   {}", processor_add_count);
    println!("  - Processor removed events: {}", processor_remove_count);
    println!("  - Link wired events:        {}", link_wire_count);
    println!("  - Link unwired events:      {}", link_unwire_count);
    println!("  - Graph change events:      {}", graph_change_count);
    println!("  - Compiler complete events: {}", compiler_count);

    // Verify expected counts (2 processors added, 2 removed, 1 link wired, 1 unwired)
    let mut passed = true;

    print!("\nVerifying processor add events (expected 2): ");
    if processor_add_count >= 2 {
        println!("PASS");
    } else {
        println!("FAIL (got {})", processor_add_count);
        passed = false;
    }

    print!("Verifying processor remove events (expected 2): ");
    if processor_remove_count >= 2 {
        println!("PASS");
    } else {
        println!("FAIL (got {})", processor_remove_count);
        passed = false;
    }

    print!("Verifying link wire events (expected 1): ");
    if link_wire_count >= 1 {
        println!("PASS");
    } else {
        println!("FAIL (got {})", link_wire_count);
        passed = false;
    }

    print!("Verifying link unwire events (expected 1): ");
    if link_unwire_count >= 1 {
        println!("PASS");
    } else {
        println!("FAIL (got {})", link_unwire_count);
        passed = false;
    }

    print!("Verifying graph change events (expected 1+): ");
    if graph_change_count >= 1 {
        println!("PASS");
    } else {
        println!("FAIL (got {})", graph_change_count);
        passed = false;
    }

    if passed {
        println!("\n✓ All WebSocket event verifications passed!");
    } else {
        println!("\n✗ Some WebSocket event verifications failed!");
    }
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
