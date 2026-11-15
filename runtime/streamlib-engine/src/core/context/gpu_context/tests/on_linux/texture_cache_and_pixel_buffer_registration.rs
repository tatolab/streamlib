// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A kernel id names a pipeline, and two pipelines built from one module
/// against different entry points are different pipelines. Serving the
/// first for the second would dispatch a function the caller did not ask
/// for.
#[test]
fn the_kernel_id_tells_two_entry_points_over_one_module_apart() {
    let spv = b"not really spir-v, and this key never parses it";
    assert_ne!(
        compute_kernel_cache_key(spv, 0, "main"),
        compute_kernel_cache_key(spv, 0, "sharpen"),
    );
}

/// #1262 OPAQUE_FD/CUDA producer surface — positive mint/export/wrap
/// path plus the zeroed-cached-fields regression.
///
/// Mental-revert: if `create_opaque_fd_export_buffer` built the
/// `StorageBuffer` with a zeroed `byte_size_cached` (the silent
/// all-zero cached size),
/// the `byte_size() == BYTES` assertion below fails immediately —
/// with no panic, no export error, just a wrong cached POD. GPU-gated:
/// skips when no device is present (CI is GPU-free).
#[test]
fn opaque_fd_export_buffer_mint_export_wrap_and_non_zero_cache() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    const W: u32 = 32;
    const H: u32 = 32;
    const BPP: u32 = 4;
    const BYTES: u64 = (W as u64) * (H as u64) * (BPP as u64);

    // HOST_VISIBLE OPAQUE_FD flavor is broadly available; the
    // DEVICE_LOCAL CUDA flavor rides the same code path with a
    // different pool. Skip if the OPAQUE_FD pool is unavailable on
    // this driver rather than failing the suite.
    let storage = match gpu.create_opaque_fd_export_buffer(BYTES, false) {
        Ok(s) => s,
        Err(e) => {
            println!("Skipping - OPAQUE_FD export pool unavailable: {e}");
            return;
        }
    };

    // Zeroed-cached-fields regression: the cached byte size must be
    // the real allocation size, never 0.
    assert_eq!(
        storage.byte_size(),
        BYTES,
        "StorageBuffer.byte_size_cached must carry the real allocation size (zeroed-cache regression)"
    );
    assert!(
        !storage.mapped_ptr().is_null(),
        "HOST_VISIBLE OPAQUE_FD buffer must expose a persistent mapping"
    );

    // Export → fresh dup'd fd + size + device UUID.
    let (fd, size, uuid) = gpu
        .export_storage_buffer_opaque_fd(&storage)
        .expect("export_storage_buffer_opaque_fd failed");
    assert!(fd >= 0, "exported OPAQUE_FD must be non-negative, got {fd}");
    assert_eq!(size, BYTES, "exported size must equal the allocation size");
    assert!(
        uuid.iter().any(|b| *b != 0),
        "device UUID must not be all-zero — CUDA device binding depends on it, got {uuid:02x?}"
    );
    // The caller owns the dup'd fd; close it so the test leaks nothing.
    unsafe { libc::close(fd) };

    // Wrap → PixelBuffer sharing the same allocation, with the
    // caller's pixel-shape metadata cached.
    let pixel_buffer = gpu
        .wrap_storage_buffer_as_pixel_buffer(
            &storage,
            W,
            H,
            BPP,
            crate::core::rhi::PixelFormat::Bgra32,
        )
        .expect("wrap_storage_buffer_as_pixel_buffer failed");
    assert_eq!(pixel_buffer.width, W);
    assert_eq!(pixel_buffer.height, H);

    println!("OPAQUE_FD mint/export/wrap OK — byte_size={BYTES} uuid={uuid:02x?}");
}

/// A privileged FullAccess plugin passing oversized dimensions (or a
/// source buffer smaller than width*height*4) must get a typed
/// `Error::GpuError` from `upload_pixel_buffer_as_texture` BEFORE any
/// `vkCmdCopyBufferToImage` submit — not a GPU-side out-of-bounds read
/// of the HOST_VISIBLE staging buffer (`VK_ERROR_DEVICE_LOST`). #1388 is
/// the first SDK/plugin exposure of this slot, so this closes the
/// device-fault mode the ABI is meant to close.
///
/// Mental-revert: drop the required-byte-size guard in
/// `upload_pixel_buffer_as_texture` and this call records a
/// 4096x4096x4 (64 MiB) tightly-packed copy region against a 64-byte
/// source buffer, faulting the device instead of returning. GPU-gated:
/// skips cleanly with no device (CI is GPU-free) — a /verify-live
/// regression.
#[test]
fn upload_pixel_buffer_as_texture_rejects_oversized_dimensions() {
    use std::sync::Arc;

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    // 4x4 RGBA8 HOST_VISIBLE source = 64 bytes.
    const SRC_W: u32 = 4;
    const SRC_H: u32 = 4;
    const SRC_BYTES: u64 = (SRC_W as u64) * (SRC_H as u64) * 4;
    let host_buffer =
        match crate::vulkan::rhi::HostVulkanBuffer::new(&gpu.device().inner, SRC_BYTES) {
            Ok(b) => b,
            Err(e) => {
                println!("Skipping - HOST_VISIBLE buffer allocation failed: {e}");
                return;
            }
        };
    let pixel_buffer = crate::core::rhi::PixelBuffer::from_host_vulkan_buffer(
        Arc::new(host_buffer),
        SRC_W,
        SRC_H,
        4,
        crate::core::rhi::PixelFormat::Rgba32,
    );

    // Oversized copy region: 4096x4096x4 = 64 MiB, far past the 64-byte
    // source. Without the guard this is a GPU out-of-bounds read of the
    // staging buffer.
    let result = gpu.upload_pixel_buffer_as_texture(
        "oversized_upload_regression",
        &pixel_buffer,
        4096,
        4096,
    );

    let err = result.expect_err(
        "oversized upload_pixel_buffer_as_texture must return a typed error, not submit a \
         GPU out-of-bounds copy",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("upload_pixel_buffer_as_texture") && msg.contains("bytes"),
        "expected a byte-size validation error, got: {msg}"
    );
    println!("upload_pixel_buffer_as_texture oversized-dimension guard OK — {msg}");
}

#[test]
fn test_register_texture_with_layout_round_trip() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let desc = TextureDescriptor::new(640, 480, TextureFormat::Rgba8Unorm)
        .with_usage(TextureUsages::TEXTURE_BINDING);
    let texture = gpu
        .device()
        .create_texture(&desc)
        .expect("texture creation failed");
    let surface_id = "test-surface-with-layout";

    gpu.register_texture_with_layout(surface_id, texture, VulkanLayout::SHADER_READ_ONLY_OPTIMAL);

    let registration = gpu
        .resolve_texture_registration_by_surface_id(surface_id, None, 640, 480)
        .expect("registration cache miss");
    assert_eq!(
        registration.current_layout(),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "declared initial layout should be visible to consumers"
    );

    // Update flow — consumer barriers transition + advance layout.
    registration.update_layout(VulkanLayout::TRANSFER_SRC_OPTIMAL);
    let registration2 = gpu
        .resolve_texture_registration_by_surface_id(surface_id, None, 640, 480)
        .expect("second resolve");
    assert_eq!(
        registration2.current_layout(),
        VulkanLayout::TRANSFER_SRC_OPTIMAL,
        "later resolves see the updated layout (Arc share)"
    );

    // Default register_texture path declares UNDEFINED.
    let texture2 = gpu
        .device()
        .create_texture(&desc)
        .expect("second texture creation failed");
    gpu.register_texture("test-surface-default-layout", texture2);
    let registration3 = gpu
        .resolve_texture_registration_by_surface_id("test-surface-default-layout", None, 640, 480)
        .expect("default-layout resolve");
    assert_eq!(
        registration3.current_layout(),
        VulkanLayout::UNDEFINED,
        "register_texture without explicit layout defaults to UNDEFINED"
    );

    println!("register_texture_with_layout + resolve_texture_registration_by_surface_id: OK");
}

/// Where the device exports no DMA-BUF, a pooled pixel buffer registers as
/// OPAQUE_FD stating its memory type index and exporting device; where it
/// does, it registers as DMA-BUF stating neither.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_pixel_buffer_registers_as_opaque_fd_with_its_memory_type_index_where_dma_buf_is_absent() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the pixel-buffer registration flavour test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let (published_frame_id, pixel_buffer) = gpu
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("a pooled pixel buffer");
    let published_frame_id = published_frame_id.to_string();
    let registration = live_service
        .state
        .get_surface_planes(pool_slot_key_of_surface_id(&published_frame_id))
        .expect("the pool registered its slot");

    let host_device = &gpu.device().inner;
    let device_name = host_device.name();
    if host_device.supports_dma_buf_export() {
        assert_eq!(registration.handle_type, "dma_buf", "{device_name}");
        assert_eq!(registration.vk_memory_type_index, None, "{device_name}");
        assert_eq!(registration.exporting_device_uuid, None, "{device_name}");
    } else {
        let exporting_device_uuid_in_lowercase_hex =
            streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(
                host_device.physical_device_uuid(),
            );
        assert_eq!(registration.handle_type, "opaque_fd", "{device_name}");
        assert_eq!(
            registration.vk_memory_type_index,
            pixel_buffer
                .buffer_ref()
                .inner
                .vma_allocation_memory_type_index(),
            "the registration states the slot's own memory type on {device_name}"
        );
        assert_eq!(
            registration.exporting_device_uuid.as_deref(),
            Some(exporting_device_uuid_in_lowercase_hex.as_str()),
            "{device_name}"
        );
    }
    live_service.stop();
}

/// A pooled pixel buffer the service registered resolves through the
/// store's `lookup_buffer` into an import of the same memory, holding the
/// bytes the producer wrote — over DMA-BUF where the device exports it,
/// over OPAQUE_FD at the stated memory type index where it does not.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_pooled_pixel_buffer_reads_back_through_lookup_buffer_with_its_bytes_intact() {
    let Ok(gpu) = GpuContext::init_for_platform() else {
        tracing::warn!("no GPU device, so the lookup_buffer read-back test skips");
        return;
    };
    let live_service = crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let slot_bytes = (LEASE_TEST_SURFACE_WIDTH * LEASE_TEST_SURFACE_HEIGHT * 4) as usize;
    let producer_pattern_byte = |index: usize| (index.wrapping_mul(31).wrapping_add(7)) as u8;

    let (published_frame_id, pixel_buffer) = gpu
        .acquire_pixel_buffer(
            LEASE_TEST_SURFACE_WIDTH,
            LEASE_TEST_SURFACE_HEIGHT,
            PixelFormat::Rgba32,
        )
        .expect("a pooled pixel buffer");
    let producer_bytes = unsafe {
        std::slice::from_raw_parts_mut(pixel_buffer.buffer_ref().inner.mapped_ptr(), slot_bytes)
    };
    for (index, byte) in producer_bytes.iter_mut().enumerate() {
        *byte = producer_pattern_byte(index);
    }

    let published_frame_id = published_frame_id.to_string();
    let looked_up = gpu
        .surface_store()
        .expect("the engine's store")
        .lookup_buffer(pool_slot_key_of_surface_id(&published_frame_id))
        .unwrap_or_else(|refusal| {
            panic!(
                "lookup_buffer resolves the slot on {}: {refusal}",
                gpu.device().inner.name()
            )
        });
    assert_ne!(
        looked_up.buffer_ref().inner.mapped_ptr(),
        pixel_buffer.buffer_ref().inner.mapped_ptr(),
        "the lookup imports the memory rather than handing back the producer's own mapping"
    );
    let imported_bytes = unsafe {
        std::slice::from_raw_parts(looked_up.buffer_ref().inner.mapped_ptr(), slot_bytes)
    };
    assert!(
        imported_bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| *byte == producer_pattern_byte(index)),
        "the imported buffer holds the producer's bytes"
    );
    drop(looked_up);
    live_service.stop();
}

/// The upload's terminal barrier — and the layout it publishes to the
/// registration — follow the destination's create-time usage.
/// `SHADER_READ_ONLY_OPTIMAL` requires `SAMPLED` or `INPUT_ATTACHMENT`
/// (VUID-VkImageMemoryBarrier2-oldLayout-01211), so a storage-only or
/// colour-attachment-only destination ends in `GENERAL` instead.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn an_upload_publishes_the_terminal_layout_its_destinations_usage_allows() {
    const UPLOAD_EXTENT: u32 = 64;

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    for (destination_shape, usage, expected_terminal_layout) in [
        (
            "storage-only — an escalate ray-tracing output being seeded",
            TextureUsages::STORAGE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
            VulkanLayout::GENERAL,
        ),
        (
            "colour-attachment-only — a scissored-draw target being seeded",
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
            VulkanLayout::GENERAL,
        ),
        (
            "sampled-capable — every ring slot and cached upload texture",
            TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        ),
    ] {
        let desc = TextureDescriptor::new(UPLOAD_EXTENT, UPLOAD_EXTENT, TextureFormat::Rgba8Unorm)
            .with_usage(usage);
        let texture = gpu
            .device()
            .create_texture_local(&desc)
            .unwrap_or_else(|e| panic!("create the {destination_shape} destination: {e}"));
        let surface_id = uuid::Uuid::new_v4().to_string();
        gpu.register_texture_with_layout(&surface_id, texture.clone(), VulkanLayout::UNDEFINED);

        let (_pool_id, pixel_buffer) = gpu
            .acquire_pixel_buffer(UPLOAD_EXTENT, UPLOAD_EXTENT, PixelFormat::Rgba32)
            .unwrap_or_else(|e| panic!("acquire the upload source: {e}"));
        gpu.copy_pixel_buffer_to_texture(
            &pixel_buffer,
            &texture,
            &surface_id,
            UPLOAD_EXTENT,
            UPLOAD_EXTENT,
        )
        .unwrap_or_else(|e| panic!("upload into the {destination_shape} destination: {e}"));

        let registration = gpu
            .resolve_texture_registration_by_surface_id(
                &surface_id,
                None,
                UPLOAD_EXTENT,
                UPLOAD_EXTENT,
            )
            .expect("the seeded destination resolves its registration");
        assert_eq!(
            registration.current_layout(),
            expected_terminal_layout,
            "a {destination_shape} destination must be published in the terminal layout its \
             usage can legally hold"
        );
    }
}

/// Seeding a destination whose usage cannot hold
/// `SHADER_READ_ONLY_OPTIMAL` must raise no validation finding at the
/// upload, and none at a consumer's barrier out of the layout that
/// upload published. The `GENERAL` transition below is that consumer —
/// the barrier an escalate ray-tracing dispatch takes to bind its
/// output as a storage descriptor.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn seeding_a_non_sampled_destination_and_consuming_it_raises_no_validation_finding() {
    use crate::host_rhi::{VulkanAccess, VulkanStage};

    const UPLOAD_EXTENT: u32 = 64;

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };
    let counts_before = gpu.device().inner.validation_layer_message_counts();
    let Some(counts_before) = counts_before else {
        println!(
            "Skipping — no validation messenger installed. Re-run with \
             STREAMLIB_VULKAN_VALIDATION=1 and VK_LAYER_KHRONOS_validation present."
        );
        return;
    };

    let desc = TextureDescriptor::new(UPLOAD_EXTENT, UPLOAD_EXTENT, TextureFormat::Rgba8Unorm)
        .with_usage(
            TextureUsages::STORAGE_BINDING | TextureUsages::COPY_DST | TextureUsages::COPY_SRC,
        );
    let texture = gpu
        .device()
        .create_texture_local(&desc)
        .expect("create the storage-only destination");
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(&surface_id, texture.clone(), VulkanLayout::UNDEFINED);

    let (_pool_id, pixel_buffer) = gpu
        .acquire_pixel_buffer(UPLOAD_EXTENT, UPLOAD_EXTENT, PixelFormat::Rgba32)
        .expect("acquire the upload source");
    gpu.copy_pixel_buffer_to_texture(
        &pixel_buffer,
        &texture,
        &surface_id,
        UPLOAD_EXTENT,
        UPLOAD_EXTENT,
    )
    .expect("seed the storage-only destination");

    let registration = gpu
        .resolve_texture_registration_by_surface_id(&surface_id, None, UPLOAD_EXTENT, UPLOAD_EXTENT)
        .expect("the seeded destination resolves its registration");
    let published_layout = registration.current_layout();

    let mut recorder = gpu
        .create_command_recorder("seeded_storage_output_into_general")
        .expect("a recorder for the consuming barrier");
    recorder.begin().expect("begin the consuming barrier");
    recorder
        .record_image_barrier(
            &texture,
            published_layout,
            VulkanLayout::GENERAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::COMPUTE_SHADER,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
            VulkanAccess::SHADER_READ | VulkanAccess::SHADER_WRITE,
        )
        .expect("barrier the seeded output to its storage-descriptor layout");
    recorder
        .submit_and_wait()
        .expect("submit the consuming barrier");

    let counts_after = gpu
        .device()
        .inner
        .validation_layer_message_counts()
        .expect("the messenger stays installed for the whole test");
    assert_eq!(
        counts_after.error_count, counts_before.error_count,
        "seeding a storage-only destination and barriering out of the layout that seed \
         published must raise no validation error"
    );
}
