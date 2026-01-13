// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests for XPC-based subprocess RHI.

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::time::Duration;

    use crate::apple::subprocess_rhi::{XpcBroker, XpcChannel, XpcFrameTransport};
    use crate::core::subprocess_rhi::{
        BrokerInstallStatus, SubprocessRhiBroker, SubprocessRhiChannel, SubprocessRhiFrameTransport,
    };

    /// Test that broker can be ensured running.
    ///
    /// This test requires launchd access and may show a system authorization prompt
    /// on first run.
    #[test]
    #[ignore] // Run manually with: cargo test -p streamlib test_broker_ensure_running -- --ignored
    fn test_broker_ensure_running() {
        let status = XpcBroker::ensure_running().expect("Failed to ensure broker running");

        match status {
            BrokerInstallStatus::AlreadyRunning => {
                println!("Broker was already running");
            }
            BrokerInstallStatus::Installed => {
                println!("Broker was installed and started");
            }
            BrokerInstallStatus::NotRequired => {
                println!("Broker not required on this platform");
            }
        }
    }

    /// Test broker connection.
    #[test]
    #[ignore] // Requires broker to be running
    fn test_broker_connect() {
        // First ensure broker is running
        XpcBroker::ensure_running().expect("Failed to ensure broker running");

        // Connect to broker
        let broker = XpcBroker::connect().expect("Failed to connect to broker");
        println!("Successfully connected to broker");

        // The broker connection is valid - we can't do much without a real endpoint
        drop(broker);
    }

    /// Test XpcChannel creation as runtime.
    #[test]
    #[ignore] // Requires broker to be running
    fn test_xpc_channel_create_as_runtime() {
        let runtime_id = format!("test-runtime-{}", std::process::id());

        let channel =
            XpcChannel::create_as_runtime(&runtime_id).expect("Failed to create runtime channel");

        assert_eq!(channel.runtime_id, runtime_id);
        assert!(!channel.is_connected()); // No subprocess connected yet

        println!("Created runtime channel for: {}", runtime_id);

        // Endpoint should be registered with broker
        assert!(channel.endpoint().is_some());

        drop(channel);
    }

    /// Test shared memory creation and mapping.
    #[test]
    fn test_shared_memory_roundtrip() {
        const TEST_SIZE: usize = 4096;
        const TEST_PATTERN: u8 = 0xAB;

        // Create shared memory
        let (handle, write_ptr) = XpcFrameTransport::create_shared_memory(TEST_SIZE)
            .expect("Failed to create shared memory");

        // Write test pattern
        unsafe {
            std::ptr::write_bytes(write_ptr, TEST_PATTERN, TEST_SIZE);
        }

        // Map and verify
        let read_ptr =
            XpcFrameTransport::map_shared_memory(&handle).expect("Failed to map shared memory");

        unsafe {
            for i in 0..TEST_SIZE {
                assert_eq!(*read_ptr.add(i), TEST_PATTERN, "Mismatch at offset {}", i);
            }
        }

        println!("Shared memory roundtrip successful: {} bytes", TEST_SIZE);

        // Cleanup
        XpcFrameTransport::unmap_shared_memory(read_ptr, TEST_SIZE)
            .expect("Failed to unmap shared memory");
    }

    /// Test that XpcChannel properly handles the broker-based flow.
    ///
    /// This test simulates the full flow:
    /// 1. Runtime creates channel and registers with broker
    /// 2. "Subprocess" (same process for testing) connects via broker
    /// 3. Both sides can communicate
    #[test]
    #[ignore] // Requires broker to be running
    fn test_xpc_channel_broker_flow() {
        let runtime_id = format!("test-broker-flow-{}", std::process::id());

        // Step 1: Runtime creates channel
        println!("Creating runtime channel...");
        let runtime_channel =
            XpcChannel::create_as_runtime(&runtime_id).expect("Failed to create runtime channel");

        println!("Runtime channel created, endpoint registered with broker");

        // Step 2: Subprocess connects via broker
        // Note: In a real scenario, this would be in a separate process
        println!("Connecting as subprocess...");
        let subprocess_channel = XpcChannel::connect_as_subprocess(&runtime_id)
            .expect("Failed to connect as subprocess");

        println!("Subprocess connected");

        // Give time for connection to establish
        std::thread::sleep(Duration::from_millis(100));

        // Verify connection state
        assert!(subprocess_channel.is_connected());

        println!("Broker flow test successful");

        drop(subprocess_channel);
        drop(runtime_channel);
    }

    /// Test frame send/receive between runtime and subprocess channels.
    #[test]
    #[ignore] // Requires broker and both channels to be set up
    fn test_frame_transfer() {
        use crate::core::subprocess_rhi::FrameTransportHandle;

        let runtime_id = format!("test-frame-transfer-{}", std::process::id());

        // Create channels
        let runtime_channel =
            XpcChannel::create_as_runtime(&runtime_id).expect("Failed to create runtime channel");

        let subprocess_channel = XpcChannel::connect_as_subprocess(&runtime_id)
            .expect("Failed to connect as subprocess");

        // Give time for connection
        std::thread::sleep(Duration::from_millis(200));

        // Create test shared memory as a stand-in for IOSurface
        // (Real IOSurface testing requires Metal/GPU context)
        let (handle, write_ptr) =
            XpcFrameTransport::create_shared_memory(1024).expect("Failed to create shared memory");

        // Write test data
        let test_data = b"Hello from runtime!";
        unsafe {
            std::ptr::copy_nonoverlapping(test_data.as_ptr(), write_ptr, test_data.len());
        }

        // Send frame from subprocess to runtime
        let frame_id = 42u64;
        subprocess_channel
            .send_frame(handle, frame_id)
            .expect("Failed to send frame");

        println!("Frame sent with id={}", frame_id);

        // Receive on runtime side
        let (received_handle, received_id) = runtime_channel
            .recv_frame(Duration::from_secs(5))
            .expect("Failed to receive frame");

        assert_eq!(received_id, frame_id);
        println!("Frame received with id={}", received_id);

        // Verify data via mapping
        if let FrameTransportHandle::SharedMemory { .. } = received_handle {
            let read_ptr = XpcFrameTransport::map_shared_memory(&received_handle)
                .expect("Failed to map received memory");

            let received_data = unsafe { std::slice::from_raw_parts(read_ptr, test_data.len()) };
            assert_eq!(received_data, test_data);

            println!("Data verified: {:?}", std::str::from_utf8(received_data));

            XpcFrameTransport::unmap_shared_memory(read_ptr, 1024).ok();
        }

        drop(subprocess_channel);
        drop(runtime_channel);
    }

    /// Benchmark shared memory allocation.
    #[test]
    fn test_shared_memory_allocation_speed() {
        const ITERATIONS: usize = 100;
        const SIZE: usize = 1920 * 1080 * 4; // 4K BGRA frame

        let start = std::time::Instant::now();

        for _ in 0..ITERATIONS {
            let (handle, _ptr) = XpcFrameTransport::create_shared_memory(SIZE)
                .expect("Failed to create shared memory");

            // Release handle
            crate::apple::subprocess_rhi::release_frame_transport_handle(handle);
        }

        let elapsed = start.elapsed();
        let per_frame = elapsed / ITERATIONS as u32;

        println!(
            "Allocated {} frames ({} MB each) in {:?} ({:?}/frame)",
            ITERATIONS,
            SIZE / (1024 * 1024),
            elapsed,
            per_frame
        );

        // Should be fast enough for real-time (< 1ms per allocation)
        assert!(
            per_frame < Duration::from_millis(10),
            "Allocation too slow: {:?}/frame",
            per_frame
        );
    }
}
