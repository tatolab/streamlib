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
        "counter" => run_counter_sender(),
        "pixels" => run_pixel_sender(),
        #[cfg(target_os = "macos")]
        "iosurface" => run_iosurface_sender(),
        _ => {
            eprintln!("Usage: sender [counter|pixels|iosurface]");
            eprintln!("  counter   - Send incrementing u64 values (default)");
            eprintln!("  pixels    - Send 1080p RGB pixel buffers (~6.2MB copy)");
            #[cfg(target_os = "macos")]
            eprintln!("  iosurface - Send IOSurface IDs only (32 bytes, zero-copy GPU)");
            std::process::exit(1);
        }
    }
}

fn run_counter_sender() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting counter sender...");

    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&"iceoryx2-test/counter".try_into()?)
        .publish_subscribe::<u64>()
        .open_or_create()?;

    let publisher = service.publisher_builder().create()?;

    let mut counter: u64 = 0;
    loop {
        let sample = publisher.loan_uninit()?;
        let sample = sample.write_payload(counter);
        sample.send()?;

        println!("Sent: {}", counter);
        counter += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn run_pixel_sender() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "Starting pixel sender ({}x{}x{} = {} bytes)...",
        WIDTH, HEIGHT, CHANNELS, BUFFER_SIZE
    );

    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&"iceoryx2-test/pixels".try_into()?)
        .publish_subscribe::<[u8]>()
        .open_or_create()?;

    let publisher = service
        .publisher_builder()
        .initial_max_slice_len(BUFFER_SIZE)
        .create()?;

    let mut frame_count: u64 = 0;
    loop {
        let start = Instant::now();

        // Loan uninitialized buffer directly in shared memory
        let sample = publisher.loan_slice_uninit(BUFFER_SIZE)?;

        // Fill with a test pattern (RGB gradient based on frame count)
        let r = ((frame_count * 3) % 256) as u8;
        let g = ((frame_count * 5) % 256) as u8;
        let b = ((frame_count * 7) % 256) as u8;

        let sample = sample.write_from_fn(|idx| match idx % 3 {
            0 => r,
            1 => g,
            _ => b,
        });

        sample.send()?;

        let elapsed = start.elapsed();
        println!(
            "Frame {}: sent {}x{}x{} ({} bytes) in {:?}",
            frame_count, WIDTH, HEIGHT, CHANNELS, BUFFER_SIZE, elapsed
        );

        frame_count += 1;
        std::thread::sleep(Duration::from_millis(16)); // ~60fps
    }
}

#[cfg(target_os = "macos")]
fn run_iosurface_sender() -> Result<(), Box<dyn std::error::Error>> {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
    use objc2_foundation::{ns_string, NSDictionary, NSNumber, NSString};
    use objc2_io_surface::IOSurface;
    use std::ffi::c_void;
    use std::time::SystemTime;

    // FFI declaration for IOSurfaceGetID
    #[link(name = "IOSurface", kind = "framework")]
    extern "C" {
        fn IOSurfaceGetID(buffer: *const c_void) -> u32;
    }

    println!(
        "Starting IOSurface sender ({}x{}, sending only 32-byte ID message)...",
        WIDTH, HEIGHT
    );

    let node = NodeBuilder::new().create::<ipc::Service>()?;

    let service = node
        .service_builder(&"iceoryx2-test/iosurface".try_into()?)
        .publish_subscribe::<IOSurfaceMessage>()
        .open_or_create()?;

    let publisher = service.publisher_builder().create()?;

    // Create a pool of IOSurfaces to simulate real video frames
    // Using the pattern from libs/streamlib/src/apple/iosurface.rs
    let surfaces: Vec<Retained<IOSurface>> = (0..3)
        .map(|_| {
            let bytes_per_element = 4usize; // BGRA
            let bytes_per_row = (WIDTH * bytes_per_element).div_ceil(64) * 64;

            let val_width = NSNumber::new_usize(WIDTH);
            let val_height = NSNumber::new_usize(HEIGHT);
            let val_pixel_format = NSNumber::new_u32(0x42475241); // 'BGRA'
            let val_bytes_per_element = NSNumber::new_usize(bytes_per_element);
            let val_bytes_per_row = NSNumber::new_usize(bytes_per_row);

            let keys: Vec<&NSString> = vec![
                ns_string!("IOSurfaceWidth"),
                ns_string!("IOSurfaceHeight"),
                ns_string!("IOSurfacePixelFormat"),
                ns_string!("IOSurfaceBytesPerElement"),
                ns_string!("IOSurfaceBytesPerRow"),
            ];

            let values: Vec<&AnyObject> = vec![
                (&*val_width as &NSNumber).as_super(),
                (&*val_height as &NSNumber).as_super(),
                (&*val_pixel_format as &NSNumber).as_super(),
                (&*val_bytes_per_element as &NSNumber).as_super(),
                (&*val_bytes_per_row as &NSNumber).as_super(),
            ];

            let properties = NSDictionary::from_slices(&keys, &values);

            let cls = IOSurface::class();
            let allocated_ptr: *mut IOSurface = unsafe { msg_send![cls, alloc] };
            let surface_ptr: *mut IOSurface =
                unsafe { msg_send![allocated_ptr, initWithProperties: &*properties] };

            unsafe { Retained::from_raw(surface_ptr) }.expect("Failed to create IOSurface")
        })
        .collect();

    println!(
        "Created {} IOSurfaces, IDs: {:?}",
        surfaces.len(),
        surfaces
            .iter()
            .map(|s| unsafe { IOSurfaceGetID(Retained::as_ptr(s) as *const c_void) })
            .collect::<Vec<_>>()
    );

    let mut frame_count: u64 = 0;
    loop {
        let start = Instant::now();

        // Round-robin through surfaces (simulating triple buffering)
        let surface = &surfaces[(frame_count as usize) % surfaces.len()];
        let surface_id = unsafe { IOSurfaceGetID(Retained::as_ptr(surface) as *const c_void) };

        let timestamp_ns = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // Send only the ID - 32 bytes total, NOT 6.2MB of pixels
        let sample = publisher.loan_uninit()?;
        let sample = sample.write_payload(IOSurfaceMessage {
            surface_id,
            frame_number: frame_count,
            width: WIDTH as u32,
            height: HEIGHT as u32,
            timestamp_ns,
        });
        sample.send()?;

        let elapsed = start.elapsed();
        println!(
            "Frame {}: sent IOSurface ID {} (32 bytes) in {:?}",
            frame_count, surface_id, elapsed
        );

        frame_count += 1;
        std::thread::sleep(Duration::from_millis(16)); // ~60fps
    }
}
