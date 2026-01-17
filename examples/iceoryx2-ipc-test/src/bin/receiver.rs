// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use iceoryx2::prelude::*;
use iceoryx2_ipc_test::{BUFFER_SIZE, CHANNELS, HEIGHT, WIDTH};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
use iceoryx2_ipc_test::IOSurfaceMessage;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("counter");

    match mode {
        "counter" => run_counter_receiver(),
        "pixels" => run_pixel_receiver(),
        #[cfg(target_os = "macos")]
        "iosurface" => run_iosurface_receiver(),
        _ => {
            eprintln!("Usage: receiver [counter|pixels|iosurface]");
            eprintln!("  counter   - Receive incrementing u64 values (default)");
            eprintln!("  pixels    - Receive 1080p RGB pixel buffers");
            #[cfg(target_os = "macos")]
            eprintln!("  iosurface - Receive IOSurface IDs and look up the surfaces");
            std::process::exit(1);
        }
    }
}

fn run_counter_receiver() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting counter receiver...");

    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&"iceoryx2-test/counter".try_into()?)
        .publish_subscribe::<u64>()
        .open_or_create()?;

    let subscriber = service.subscriber_builder().create()?;

    loop {
        while let Some(sample) = subscriber.receive()? {
            println!("Received: {}", *sample);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn run_pixel_receiver() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Starting pixel receiver (expecting {}x{}x{} = {} bytes)...",
        WIDTH, HEIGHT, CHANNELS, BUFFER_SIZE
    );

    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&"iceoryx2-test/pixels".try_into()?)
        .publish_subscribe::<[u8]>()
        .open_or_create()?;

    let subscriber = service.subscriber_builder().create()?;

    let mut last_print = Instant::now();
    let mut frames_received = 0u64;

    loop {
        while let Some(sample) = subscriber.receive()? {
            let payload = sample.payload();

            // Verify we got the right size
            assert_eq!(payload.len(), BUFFER_SIZE);

            // Sample a few pixels to verify data integrity
            let r = payload[0];
            let g = payload[1];
            let b = payload[2];

            frames_received += 1;

            if last_print.elapsed() > Duration::from_secs(1) {
                println!(
                    "Received {} frames/sec, last frame RGB=({},{},{}), size={}",
                    frames_received,
                    r,
                    g,
                    b,
                    payload.len()
                );
                frames_received = 0;
                last_print = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_micros(100)); // Poll frequently
    }
}

#[cfg(target_os = "macos")]
fn run_iosurface_receiver() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::c_void;
    use std::time::SystemTime;

    // FFI declarations for IOSurface lookup
    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        fn IOSurfaceLookup(csid: u32) -> *const c_void;
        fn IOSurfaceGetWidth(buffer: *const c_void) -> usize;
        fn IOSurfaceGetHeight(buffer: *const c_void) -> usize;
        fn IOSurfaceGetBytesPerRow(buffer: *const c_void) -> usize;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    println!("Starting IOSurface receiver...");

    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&"iceoryx2-test/iosurface".try_into()?)
        .publish_subscribe::<IOSurfaceMessage>()
        .open_or_create()?;

    let subscriber = service.subscriber_builder().create()?;

    let mut last_print = Instant::now();
    let mut frames_received = 0u64;
    let mut total_latency_ns = 0u64;
    let mut successful_lookups = 0u64;
    let mut failed_lookups = 0u64;

    loop {
        while let Some(sample) = subscriber.receive()? {
            let msg = sample.payload();
            let receive_time = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64;

            // Calculate IPC latency (send to receive)
            let latency_ns = receive_time.saturating_sub(msg.timestamp_ns);
            total_latency_ns += latency_ns;

            // Look up the IOSurface by ID - this is the key operation!
            // If it works, we have access to the same GPU texture as the sender
            let surface_ptr = unsafe { IOSurfaceLookup(msg.surface_id) };

            if !surface_ptr.is_null() {
                // Verify we can read properties from the looked-up surface
                let width = unsafe { IOSurfaceGetWidth(surface_ptr) };
                let height = unsafe { IOSurfaceGetHeight(surface_ptr) };
                let bytes_per_row = unsafe { IOSurfaceGetBytesPerRow(surface_ptr) };

                // Release the surface (we got a retained reference from IOSurfaceLookup)
                unsafe { CFRelease(surface_ptr) };

                if width == msg.width as usize && height == msg.height as usize {
                    successful_lookups += 1;
                } else {
                    println!(
                        "WARNING: Size mismatch! Expected {}x{}, got {}x{}",
                        msg.width, msg.height, width, height
                    );
                    failed_lookups += 1;
                }

                frames_received += 1;

                if last_print.elapsed() > Duration::from_secs(1) {
                    let avg_latency_us = if frames_received > 0 {
                        (total_latency_ns / frames_received) / 1000
                    } else {
                        0
                    };

                    println!(
                        "Received {} frames/sec | Avg IPC latency: {}us | Surface {}x{} (stride={}) | Lookups: {} ok, {} failed",
                        frames_received,
                        avg_latency_us,
                        width,
                        height,
                        bytes_per_row,
                        successful_lookups,
                        failed_lookups
                    );
                    frames_received = 0;
                    total_latency_ns = 0;
                    last_print = Instant::now();
                }
            } else {
                failed_lookups += 1;
                println!(
                    "FAILED to look up IOSurface ID {} (frame {})",
                    msg.surface_id, msg.frame_number
                );
            }
        }
        std::thread::sleep(Duration::from_micros(100)); // Poll frequently
    }
}
