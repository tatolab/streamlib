// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-language iceoryx2 validation test (Rust side).
//!
//! Publishes a FramePayload with known data, then waits for an echo
//! response from the Python side. Validates:
//! 1. Size of FramePayload matches between Rust and Python
//! 2. Rust→Python data integrity
//! 3. Python→Rust data integrity (echo round-trip)
//!
//! Usage:
//!   cargo run -p iceoryx2-cross-language-test
//!
//! In another terminal:
//!   python tests/iceoryx2-cross-language/python/echo_test.py

use iceoryx2::prelude::*;
use streamlib::iceoryx2::{FramePayload, PortKey, SchemaName};

fn main() {
    println!("=== iceoryx2 Cross-Language Validation Test (Rust) ===");
    println!();

    // Print struct sizes for comparison with Python
    println!("Struct sizes:");
    println!("  PortKey:      {} bytes", std::mem::size_of::<PortKey>());
    println!(
        "  SchemaName:   {} bytes",
        std::mem::size_of::<SchemaName>()
    );
    println!(
        "  FramePayload: {} bytes",
        std::mem::size_of::<FramePayload>()
    );
    println!();

    // Create iceoryx2 node
    let node = NodeBuilder::new()
        .create::<ipc::Service>()
        .expect("Failed to create node");

    // Create publisher service (Rust publishes to Python)
    let publish_service_name =
        ServiceName::new("streamlib/test-cross-lang-rust-to-python").expect("Invalid service name");
    let publish_service = node
        .service_builder(&publish_service_name)
        .publish_subscribe::<FramePayload>()
        .open_or_create()
        .expect("Failed to create publish service");
    let publisher = publish_service
        .publisher_builder()
        .create()
        .expect("Failed to create publisher");

    // Create subscriber service (Python publishes echo back to Rust)
    let subscribe_service_name =
        ServiceName::new("streamlib/test-cross-lang-python-to-rust").expect("Invalid service name");
    let subscribe_service = node
        .service_builder(&subscribe_service_name)
        .publish_subscribe::<FramePayload>()
        .open_or_create()
        .expect("Failed to create subscribe service");
    let subscriber = subscribe_service
        .subscriber_builder()
        .create()
        .expect("Failed to create subscriber");

    // Build test payload
    let test_data = rmp_serde::to_vec_named(&serde_json::json!({"hello": "world", "count": 42}))
        .expect("Failed to serialize test data");

    let payload = FramePayload::new("test_port", "test_schema", 12345, &test_data);

    println!("Publishing test payload:");
    println!("  port_key:     '{}'", payload.port());
    println!("  schema_name:  '{}'", payload.schema());
    println!("  timestamp_ns: {}", payload.timestamp_ns);
    println!("  data_len:     {} bytes", payload.len);
    println!();

    println!("Publishing test payload (repeating every 1s until Python responds)...");
    println!("(Run: python tests/iceoryx2-cross-language/python/echo_test.py)");
    println!();

    // Publish repeatedly and check for echo response
    let timeout = std::time::Duration::from_secs(60);
    let poll_interval = std::time::Duration::from_millis(100);
    let publish_interval = std::time::Duration::from_secs(1);
    let start = std::time::Instant::now();
    let mut last_publish = std::time::Instant::now() - publish_interval; // publish immediately

    loop {
        // Re-publish periodically so Python can receive even if it starts late
        if last_publish.elapsed() >= publish_interval {
            let sample = publisher.loan_uninit().expect("Failed to loan sample");
            let sample = sample.write_payload(payload);
            sample.send().expect("Failed to send sample");
            last_publish = std::time::Instant::now();
        }

        match subscriber.receive() {
            Ok(Some(sample)) => {
                let echo = sample.payload();
                println!("Received echo from Python:");
                println!("  port_key:     '{}'", echo.port());
                println!("  schema_name:  '{}'", echo.schema());
                println!("  timestamp_ns: {}", echo.timestamp_ns);
                println!("  data_len:     {} bytes", echo.len);
                println!();

                // Validate
                let mut passed = true;

                if echo.port() != "test_port" {
                    println!(
                        "FAIL: port_key mismatch: expected 'test_port', got '{}'",
                        echo.port()
                    );
                    passed = false;
                }
                if echo.schema() != "test_schema" {
                    println!(
                        "FAIL: schema_name mismatch: expected 'test_schema', got '{}'",
                        echo.schema()
                    );
                    passed = false;
                }
                if echo.timestamp_ns != 12345 {
                    println!(
                        "FAIL: timestamp_ns mismatch: expected 12345, got {}",
                        echo.timestamp_ns
                    );
                    passed = false;
                }
                if echo.data() != test_data.as_slice() {
                    println!(
                        "FAIL: data mismatch: expected {} bytes, got {} bytes",
                        test_data.len(),
                        echo.len
                    );
                    passed = false;
                }

                if passed {
                    println!("=== ALL TESTS PASSED ===");
                } else {
                    println!("=== SOME TESTS FAILED ===");
                    std::process::exit(1);
                }
                break;
            }
            Ok(None) => {
                // No data yet
            }
            Err(e) => {
                eprintln!("Error receiving: {:?}", e);
                std::process::exit(1);
            }
        }

        if start.elapsed() > timeout {
            eprintln!(
                "TIMEOUT: No echo received from Python within {}s",
                timeout.as_secs()
            );
            std::process::exit(1);
        }

        std::thread::sleep(poll_interval);
    }
}
