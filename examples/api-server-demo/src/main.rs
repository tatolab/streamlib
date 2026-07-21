// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! API Server Control-Plane Demo
//!
//! Exercises every REST endpoint of the runtime's control plane plus the
//! `/ws/events` WebSocket event stream, from an async `#[tokio::main]` driver.
//!
//! The api-server is the runtime's HTTP + WebSocket control plane: it is
//! statically linked into `streamlib-runtime` and served in-process, not a
//! loadable plugin, so an app cannot instantiate it from the SDK. This demo is
//! therefore self-contained the same way the `api-server` probe is — it spawns
//! a real `streamlib-runtime` subprocess, waits for its control plane to answer
//! `GET /health`, drives it over reqwest + tokio-tungstenite, then tears the
//! subprocess down. Run `./setup.sh` once (it builds the runtime binary,
//! records its path for `cargo run`, and links `@tatolab/debug-utilities` so
//! the dynamic-registry POST can resolve `SimplePassthroughProcessor`).

use futures_util::StreamExt;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

type DemoResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Kills the spawned `streamlib-runtime` when the demo ends (success or error).
struct RuntimeSubprocessGuard(Child);

impl Drop for RuntimeSubprocessGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Grab an ephemeral port the OS reports free, then release it so the runtime
/// can bind it. The api-server binds the requested port and increments on
/// collision, so a freshly-vacated port is the reliable choice.
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Read the api-server's persisted bearer token from the runtime's isolated
/// home. The four mutating control-plane routes are gated behind
/// `Authorization: Bearer <token>`; the api-server auto-generates the token at
/// `0600` under `<STREAMLIB_HOME>/.streamlib/api-server/auth-token` on first
/// setup, so it exists by the time `/health` answers.
fn read_auth_token(runtime_home: &std::path::Path) -> DemoResult<String> {
    let token_path = runtime_home
        .join(".streamlib")
        .join("api-server")
        .join("auth-token");
    let token = std::fs::read_to_string(&token_path).map_err(|e| {
        format!(
            "failed to read api-server bearer token at {}: {e}",
            token_path.display()
        )
    })?;
    Ok(token.trim().to_string())
}

/// Poll `GET /health` until it returns a success status or the deadline passes.
async fn wait_for_health(client: &reqwest::Client, base_url: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(resp) = client.get(format!("{base_url}/health")).send().await {
            if resp.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

#[tokio::main]
async fn main() -> DemoResult<()> {
    println!("=== API Server Control-Plane Demo ===\n");

    // The runtime binary path is recorded by `./setup.sh` into this app's
    // `.cargo/config.toml [env]`, so `cargo run` finds it. Fail loudly with the
    // fix if it is missing.
    let runtime_bin = std::env::var("STREAMLIB_RUNTIME_BIN").map_err(|_| {
        "STREAMLIB_RUNTIME_BIN is not set — run ./setup.sh first (it builds \
         streamlib-runtime and records its path for `cargo run`)"
    })?;

    let port = free_port()?;
    let base_url = format!("http://127.0.0.1:{port}");
    let ws_url = format!("ws://127.0.0.1:{port}/ws/events");

    // Isolate the runtime's home so the demo leaves nothing behind.
    let runtime_home = std::env::temp_dir().join(format!("api-server-demo-{port}"));
    let _ = std::fs::remove_dir_all(&runtime_home);

    println!("Spawning streamlib-runtime ({runtime_bin}) on 127.0.0.1:{port} ...");
    let child = Command::new(&runtime_bin)
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env("STREAMLIB_HOME", &runtime_home)
        // Point module discovery at this app's `streamlib_modules/` (linked by
        // `./setup.sh`) so the API's dynamic-registry POST can resolve
        // `SimplePassthroughProcessor` from `@tatolab/debug-utilities`.
        .env("STREAMLIB_MODULES_DIR", env!("CARGO_MANIFEST_DIR"))
        .spawn()?;
    let runtime_guard = RuntimeSubprocessGuard(child);

    let client = reqwest::Client::new();

    // Boot includes GPU init plus a first-load source build of the linked
    // package, so allow a generous window before the control plane answers.
    println!("Waiting for the control plane at {base_url}/health ...");
    if !wait_for_health(&client, &base_url, Duration::from_secs(90)).await {
        drop(runtime_guard);
        let _ = std::fs::remove_dir_all(&runtime_home);
        return Err("streamlib-runtime did not serve /health in time".into());
    }
    println!("Control plane is up.\n");

    // The four mutating routes are gated behind bearer-token auth; read the
    // token the runtime persisted under its isolated home so the mutating
    // requests below can present `Authorization: Bearer <token>`.
    let auth_token = match read_auth_token(&runtime_home) {
        Ok(token) => token,
        Err(e) => {
            drop(runtime_guard);
            let _ = std::fs::remove_dir_all(&runtime_home);
            return Err(e);
        }
    };

    // Start WebSocket event collector as async task
    let events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let ws_events = Arc::clone(&events);
    let ws_stop = Arc::new(Mutex::new(false));
    let ws_stop_flag = Arc::clone(&ws_stop);
    let ws_url_task = ws_url.clone();

    let ws_handle = tokio::spawn(async move {
        collect_websocket_events(ws_url_task, ws_events, ws_stop_flag).await;
    });

    // Give WebSocket a moment to connect
    tokio::time::sleep(Duration::from_millis(200)).await;

    println!("\n--- Running REST API Tests ---\n");

    // Test 1: Health endpoint
    test_health(&client, &base_url).await;

    // Test 2: Get registry
    test_registry(&client, &base_url).await;

    // Test 3: Get initial graph
    test_get_graph(&client, &base_url, "Initial graph").await;

    // Test 4: Create a processor
    let processor_id = test_create_processor(&client, &base_url, &auth_token).await;

    // Test 5: Verify processor in graph
    test_get_graph(&client, &base_url, "After adding processor").await;

    // Test 6: Create another processor for connection test
    let processor_id_2 = test_create_processor_2(&client, &base_url, &auth_token).await;

    // Test 7: Create connection between processors
    let connection_id =
        test_create_connection(&client, &base_url, &processor_id, &processor_id_2, &auth_token)
            .await;

    // Test 8: Verify connection in graph
    test_get_graph(&client, &base_url, "After adding connection").await;

    // Test 9: Delete connection
    test_delete_connection(&client, &base_url, &connection_id, &auth_token).await;

    // Test 10: Verify connection removed
    test_get_graph(&client, &base_url, "After deleting connection").await;

    // Test 11: Delete processors
    test_delete_processor(&client, &base_url, &processor_id, &auth_token).await;
    test_delete_processor(&client, &base_url, &processor_id_2, &auth_token).await;

    // Test 12: Verify processors removed
    test_get_graph(&client, &base_url, "After deleting processors").await;

    println!("\n--- REST API Tests Complete ---\n");

    // Give events time to be delivered
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Stop WebSocket collector
    *ws_stop.lock().await = true;
    let _ = ws_handle.await;

    // Verify WebSocket events
    println!("--- Verifying WebSocket Events ---\n");
    verify_websocket_events(&events).await;

    println!("\n--- All Tests Complete ---\n");

    // Tear the runtime subprocess down and clean up its isolated home.
    drop(runtime_guard);
    let _ = std::fs::remove_dir_all(&runtime_home);

    Ok(())
}

/// Collect WebSocket events in background using async tokio-tungstenite
async fn collect_websocket_events(
    ws_url: String,
    events: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<Mutex<bool>>,
) {
    let ws_stream = match tokio_tungstenite::connect_async(&ws_url).await {
        Ok((stream, _response)) => stream,
        Err(e) => {
            eprintln!("WebSocket connection error: {}", e);
            return;
        }
    };

    println!("WebSocket connected to {}", ws_url);

    let (_write, mut read) = ws_stream.split();

    loop {
        // Check stop flag
        if *stop.lock().await {
            break;
        }

        // Use tokio::select! to poll both the stop flag and incoming messages
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                            events.lock().await.push(json);
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Err(e)) => {
                        if !*stop.lock().await {
                            eprintln!("WebSocket read error: {}", e);
                        }
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                // Periodic check for stop flag
            }
        }
    }
}

/// Verify that expected events were received via WebSocket
async fn verify_websocket_events(events: &Arc<Mutex<Vec<serde_json::Value>>>) {
    let events = events.lock().await;
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

async fn test_health(client: &reqwest::Client, base_url: &str) {
    print!("Testing GET /health ... ");
    let resp = client.get(format!("{}/health", base_url)).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let body = r.text().await.unwrap_or_default();
            println!("OK ({})", body);
        }
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

async fn test_registry(client: &reqwest::Client, base_url: &str) {
    print!("Testing GET /api/registry ... ");
    let resp = client
        .get(format!("{}/api/registry", base_url))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
            let count = json["processors"].as_array().map(|a| a.len()).unwrap_or(0);
            println!("OK ({} processors registered)", count);
        }
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

async fn test_get_graph(client: &reqwest::Client, base_url: &str, label: &str) {
    print!("Testing GET /api/graph ({}) ... ", label);
    let resp = client.get(format!("{}/api/graph", base_url)).send().await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
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

async fn test_create_processor(
    client: &reqwest::Client,
    base_url: &str,
    auth_token: &str,
) -> String {
    print!("Testing POST /api/processor (SimplePassthroughProcessor) ... ");
    let body = serde_json::json!({
        "processor_type": "SimplePassthroughProcessor",
        "config": {}
    });
    let resp = client
        .post(format!("{}/api/processor", base_url))
        .bearer_auth(auth_token)
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
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

async fn test_create_processor_2(
    client: &reqwest::Client,
    base_url: &str,
    auth_token: &str,
) -> String {
    print!("Testing POST /api/processor (SimplePassthroughProcessor #2) ... ");
    let body = serde_json::json!({
        "processor_type": "SimplePassthroughProcessor",
        "config": {}
    });
    let resp = client
        .post(format!("{}/api/processor", base_url))
        .bearer_auth(auth_token)
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
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

async fn test_create_connection(
    client: &reqwest::Client,
    base_url: &str,
    from_processor: &str,
    to_processor: &str,
    auth_token: &str,
) -> String {
    print!("Testing POST /api/connections ... ");
    let body = serde_json::json!({
        "from_processor": from_processor,
        "from_port": "output",
        "to_processor": to_processor,
        "to_port": "input"
    });
    let resp = client
        .post(format!("{}/api/connections", base_url))
        .bearer_auth(auth_token)
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            let json: serde_json::Value = r.json().await.unwrap_or_default();
            let id = json["id"].as_str().unwrap_or("unknown").to_string();
            println!("OK (id: {})", id);
            id
        }
        Ok(r) => {
            let body = r.text().await.unwrap_or_default();
            println!("FAIL (status: {}, body: {})", body, body);
            String::new()
        }
        Err(e) => {
            println!("FAIL ({})", e);
            String::new()
        }
    }
}

async fn test_delete_connection(
    client: &reqwest::Client,
    base_url: &str,
    connection_id: &str,
    auth_token: &str,
) {
    print!("Testing DELETE /api/connections/{} ... ", connection_id);
    let resp = client
        .delete(format!("{}/api/connections/{}", base_url, connection_id))
        .bearer_auth(auth_token)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => println!("OK"),
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}

async fn test_delete_processor(
    client: &reqwest::Client,
    base_url: &str,
    processor_id: &str,
    auth_token: &str,
) {
    print!("Testing DELETE /api/processors/{} ... ", processor_id);
    let resp = client
        .delete(format!("{}/api/processors/{}", base_url, processor_id))
        .bearer_auth(auth_token)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => println!("OK"),
        Ok(r) => println!("FAIL (status: {})", r.status()),
        Err(e) => println!("FAIL ({})", e),
    }
}
