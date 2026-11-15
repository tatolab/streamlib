// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_texture_cache_register_and_resolve() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    // Create a texture and register it
    let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::TEXTURE_BINDING);
    let texture = gpu
        .device()
        .create_texture(&desc)
        .expect("texture creation failed");
    let surface_id = "test-surface-001";

    gpu.register_texture(surface_id, texture.clone());

    let resolved = gpu
        .resolve_texture_by_surface_id(surface_id, None, 640, 480)
        .expect("texture cache miss");
    assert_eq!(resolved.width(), 640);
    assert_eq!(resolved.height(), 480);

    println!("Texture cache: register + resolve OK");
}

/// A same-process producer that publishes only a pixel buffer (no
/// texture registration) must resolve through the pool's local cache —
/// the cross-process surface store cannot serve OPAQUE_FD-backed buffers
/// to a host-side consumer at all. Mental-revert: removing the
/// pool-cache arm in `resolve_texture_registration_by_surface_id`'s
/// Path 3 makes this test fail with "No texture or pixel buffer found".
/// GPU-gated: skips when no device is present.
#[test]
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn same_process_pixel_buffer_resolves_without_the_surface_store() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let (pool_id, pixel_buffer) = gpu
        .acquire_pixel_buffer(16, 16, PixelFormat::Rgba32)
        .expect("acquire pixel buffer");

    let registration = gpu
        .resolve_texture_registration_by_surface_id(&pool_id.to_string(), None, 16, 16)
        .expect("pixel-buffer-only surface must resolve same-process");
    assert_eq!(registration.texture().width(), 16);
    assert_eq!(registration.texture().height(), 16);
    assert_eq!(
        registration.current_layout(),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "the refresh upload leaves the texture shader-readable"
    );
    drop(pixel_buffer);
}

#[test]
fn test_texture_cache_miss() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    // Cache miss returns error (no texture registered, no surface-share service)
    assert!(
        gpu.resolve_texture_by_surface_id("nonexistent-surface", None, 640, 480)
            .is_err()
    );

    println!("Texture cache miss: OK");
}
