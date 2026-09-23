// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Helper-process fixture for the IOSurface texture crossing test.
//!
//! `read-back <surface_id>` checks the texture out over the surface-share Mach
//! channel, imports its IOSurface as an image and its timeline pair as shared
//! events in its own Vulkan device, waits for the engine's `produce_done` at
//! 1, reads the pixels back through the surface at its stride, and releases
//! the texture by signalling `consume_done` at 1.
//!
//! Reports to the parent on stdout, one line per event.

#![allow(clippy::disallowed_macros)]

/// The Mach channel and IOSurface are macOS-only, so elsewhere the binary
/// exists to keep the target compiling and nothing drives it.
#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let surface_id = arguments.get(2).map(String::as_str).unwrap_or_default();
    match arguments.get(1).map(String::as_str) {
        Some("read-back") => texture_helper::read_back(surface_id),
        other => {
            eprintln!("iosurface_texture_helper: unknown role {other:?}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "macos")]
#[path = "../support/iosurface_texture_test_pattern.rs"]
mod iosurface_texture_test_pattern;

#[cfg(target_os = "macos")]
mod texture_helper {
    use std::io::Write as _;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::iosurface_texture_test_pattern::engine_pattern_byte;
    use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
    use streamlib_consumer_rhi::{
        ConsumerVulkanDevice, ConsumerVulkanTexture, ConsumerVulkanTimelineSemaphore, TextureFormat,
    };
    use streamlib_surface_client::{
        SURFACE_SHARE_HAS_PRODUCE_DONE_PORT, SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE,
        SurfaceShareMachServiceConnection,
    };

    const PRODUCE_DONE_WAIT_BUDGET_NS: u64 = 20_000_000_000;

    fn report(line: &str) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    fn fail(line: &str) -> ! {
        report(line);
        std::process::exit(3);
    }

    fn stated_u64(answer: &serde_json::Value, key: &str) -> u64 {
        answer[key]
            .as_u64()
            .unwrap_or_else(|| fail(&format!("NO_{key} {answer}")))
    }

    pub fn read_back(surface_id: &str) {
        let service_name = std::env::var(SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE)
            .expect("the service name in the environment");
        let connection =
            SurfaceShareMachServiceConnection::connect(&service_name, Duration::from_secs(10))
                .unwrap_or_else(|failure| fail(&format!("CONNECT_FAILED {failure}")));
        let (answer, ports) = connection
            .send_request_with_ports(
                &serde_json::json!({"op": "check_out", "surface_id": surface_id}),
                Vec::new(),
            )
            .expect("check_out round-trip");
        if let Some(refusal) = answer.get("error") {
            fail(&format!("CHECK_OUT_REFUSED {refusal}"));
        }
        if answer["resource_type"] != "texture"
            || answer["handle_type"] != "iosurface"
            || answer[SURFACE_SHARE_HAS_PRODUCE_DONE_PORT] != true
            || ports.len() != 3
        {
            fail(&format!("NOT_A_TEXTURE_WITH_ITS_PAIR {answer}"));
        }
        let (width, height) = (
            stated_u64(&answer, "width") as u32,
            stated_u64(&answer, "height") as u32,
        );
        let format = answer["format"]
            .as_str()
            .and_then(TextureFormat::from_wire_name)
            .unwrap_or_else(|| fail(&format!("NO_FORMAT {answer}")));
        let usage = stated_u64(&answer, "vk_image_usage") as u32;

        let device = Arc::new(
            ConsumerVulkanDevice::new()
                .unwrap_or_else(|failure| fail(&format!("NO_DEVICE {failure}"))),
        );
        let iosurface = IOSurfaceRef::lookup_from_mach_port(ports[0].as_raw_name())
            .unwrap_or_else(|| fail("THE_PORT_NAMES_NO_IOSURFACE"));
        let texture = ConsumerVulkanTexture::from_iosurface(
            &device, &iosurface, width, height, format, usage,
        )
        .unwrap_or_else(|failure| fail(&format!("IMAGE_IMPORT_FAILED {failure}")));
        let import_timeline = |edge: usize| {
            ConsumerVulkanTimelineSemaphore::from_imported_metal_shared_event_mach_send_right(
                &device,
                &ports[edge],
            )
            .unwrap_or_else(|failure| fail(&format!("TIMELINE_IMPORT_FAILED {failure}")))
        };
        let (produce_done, consume_done) = (import_timeline(1), import_timeline(2));
        report(&format!(
            "IMPORTED layout={} tiling={}",
            answer["current_image_layout"], answer["vk_image_tiling"]
        ));

        produce_done
            .wait(1, PRODUCE_DONE_WAIT_BUDGET_NS)
            .unwrap_or_else(|failure| fail(&format!("PRODUCE_DONE_WAIT_FAILED {failure}")));
        let row_byte_len = (width * format.bytes_per_pixel()) as usize;
        let surface = texture
            .backing_iosurface()
            .unwrap_or_else(|| fail("THE_IMAGE_HOLDS_NO_SURFACE"));
        // SAFETY: a read-only lock on a surface this process holds.
        let locked = unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        if locked != 0 {
            fail(&format!("LOCK_FAILED {locked}"));
        }
        let base = surface.base_address().as_ptr().cast::<u8>();
        let mut mismatches = 0usize;
        for row in 0..height as usize {
            // SAFETY: the row lies inside the locked surface, at its own stride.
            let row_bytes = unsafe {
                std::slice::from_raw_parts(base.add(row * surface.bytes_per_row()), row_byte_len)
            };
            for (column, byte) in row_bytes.iter().enumerate() {
                if *byte != engine_pattern_byte(row * row_byte_len + column) {
                    mismatches += 1;
                }
            }
        }
        // SAFETY: pairs the lock above.
        unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        report(&format!("READ mismatches={mismatches}"));

        consume_done
            .signal_host(1)
            .unwrap_or_else(|failure| fail(&format!("RELEASE_FAILED {failure}")));
        report("RELEASED");
    }
}
