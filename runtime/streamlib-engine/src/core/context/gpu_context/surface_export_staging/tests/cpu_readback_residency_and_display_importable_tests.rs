// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! CPU-readback stagings, one allocation per residency, refill and write-back refusals, timeline edges, and display-importable images a viewer reads.

use super::*;

/// The bytes of a host-visible staging, read the way its consumer
/// does: straight off the mapping, with no device-to-host hop. That
/// this is a plain pointer read *is* the residency's whole point.
fn read_cpu_readback_staging_through_its_mapping(staging: &SurfaceExportStaging) -> Vec<u8> {
    let mapped = staging
        .staging_buffer()
        .expect("a buffer staging")
        .mapped_ptr();
    assert!(
        !mapped.is_null(),
        "a host-visible staging must be mapped; a null pointer here means the residency \
         fell back to device-local memory no CPU consumer can read"
    );
    unsafe { std::slice::from_raw_parts(mapped, staging.staging_byte_size() as usize) }.to_vec()
}

/// The payoff: a CPU consumer reads a texture-backed frame's pixels
/// off the mapping, with nothing installed on the context.
///
/// Mental-revert: mint this staging `DeviceLocal` and `mapped_ptr()`
/// is null, which the reader above fails on by name. Verified.
/// GPU-gated: skips when no device is present.
#[test]
fn a_cpu_readback_of_a_texture_backed_surface_lands_its_pixels_on_the_mapping() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    let kernel_output_texture = create_producer_owned_texture(&gpu);
    gpu.register_texture_with_layout(
        &surface_id,
        kernel_output_texture.clone(),
        VulkanLayout::UNDEFINED,
    );

    let (_, upload_source) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the kernel output's upload source");
    stamp_every_byte_of_pooled_backing(&upload_source, KERNEL_OUTPUT_STAMP);
    gpu.copy_pixel_buffer_to_texture(
        &upload_source,
        &kernel_output_texture,
        &surface_id,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
    )
    .expect("render into the kernel output texture");

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("a cpu-readback staging needs nothing installed");
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("refill the cpu-readback staging");

    assert_every_byte_is(
        &read_cpu_readback_staging_through_its_mapping(&staging),
        KERNEL_OUTPUT_STAMP,
        "the cpu readback of a texture-backed surface",
    );
}

/// A CPU edit written through the mapping publishes back into the
/// surface's own pooled allocation.
/// GPU-gated: skips when no device is present.
#[test]
fn a_cpu_edit_through_the_mapping_publishes_back_into_the_pooled_backing() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a pool-only frame");
    let surface_id = pool_id.to_string();
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("a cpu-readback staging for a pool-only frame");
    assert!(
        staging.writable(),
        "a frame whose only backing is its own pooled allocation is writable"
    );
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("refill before editing");

    let mapped = staging
        .staging_buffer()
        .expect("a buffer staging")
        .mapped_ptr();
    assert!(!mapped.is_null(), "a host-visible staging must be mapped");
    unsafe {
        std::ptr::write_bytes(
            mapped,
            CONSUMER_EDIT_STAMP,
            staging.staging_byte_size() as usize,
        )
    };

    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("publish the CPU edit");
    assert_every_byte_is(
        &read_pooled_backing_bytes(&pooled_backing),
        CONSUMER_EDIT_STAMP,
        "the pooled backing after a CPU edit was published",
    );
}

/// The two residencies are two allocations under one surface, and
/// asking twice at one residency is a cache hit.
///
/// A shared allocation would hand a CUDA consumer host-visible memory
/// — or a CPU consumer memory it cannot map — depending only on which
/// of them asked first.
/// GPU-gated: skips when no device is present.
#[test]
fn each_residency_is_its_own_allocation_and_repeats_hit_the_cache() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (pool_id, _pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a frame to export both ways");
    let surface_id = pool_id.to_string();

    let host_visible = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("the cpu-readback staging");
    let device_local = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("the device-export staging");

    assert!(
        !Arc::ptr_eq(&host_visible, &device_local),
        "one surface's two residencies must be two allocations"
    );
    assert!(
        !host_visible
            .staging_buffer()
            .expect("a buffer staging")
            .mapped_ptr()
            .is_null(),
        "the host-visible residency is mapped"
    );

    // Every consumer of this residency reads the mapping, so it must
    // come off the cached pool wherever the device has one. On a
    // device without, it degrades to the write-combined pool — slower
    // to read, never refused, and never a third residency.
    let vulkan_device = gpu.device().vulkan_device();
    if vulkan_device.opaque_fd_buffer_pool_host_cached().is_some() {
        assert_eq!(
            host_visible
                .staging_buffer()
                .expect("a buffer staging")
                .vma_allocation_is_host_cached(),
            Some(true),
            "a device with a cached exportable type must mint the HostVisible \
             residency from the cached pool"
        );
    }

    let host_visible_again = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("the cpu-readback staging, again");
    assert!(
        Arc::ptr_eq(&host_visible, &host_visible_again),
        "a second ask at the same residency must hit the cache, not allocate a second \
         staging the first consumer is not refilling"
    );
}

/// A refill naming a surface this staging was not opened for is
/// refused, and the refusal names that surface.
///
/// The only cover the cross-surface guard has: without it a staging
/// would answer with whatever picture its own slot happens to hold,
/// which reads as a successful copy of the wrong frame.
/// GPU-gated: skips when no device is present.
#[test]
fn a_refill_of_a_surface_this_staging_does_not_export_is_refused_by_name() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (staged_pool_id, _staged_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the frame the staging is opened for");
    let (other_pool_id, _other_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a second, unrelated frame");
    let staged_surface_id = staged_pool_id.to_string();
    let other_surface_id = other_pool_id.to_string();
    assert_ne!(
        crate::core::rhi::pool_slot_key_of_surface_id(&staged_surface_id),
        crate::core::rhi::pool_slot_key_of_surface_id(&other_surface_id),
        "the fixture needs two distinct pool slots to cross the staging against"
    );

    let staging = gpu
        .surface_export_staging(
            &staged_surface_id,
            SurfaceExportStagingResidency::HostVisible,
        )
        .expect("the cpu-readback staging");

    match gpu.refill_surface_export_staging(&staging, &other_surface_id) {
        Err(refusal) => assert!(
            refusal.to_string().contains(&other_surface_id),
            "the refusal must name the surface asked for, got: {refusal}"
        ),
        Ok(_) => panic!("a staging must never carry another surface's pixels"),
    }
}

/// A staging nobody has read a frame into holds whatever the
/// allocator handed back. Publishing it would replace a live frame's
/// picture with uninitialised memory.
///
/// Reachable only because this change made it so: at main the op
/// answered "no bridge registered" and touched nothing.
/// GPU-gated: skips when no device is present.
#[test]
fn a_write_back_of_a_never_filled_staging_is_refused_before_it_touches_the_frame() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a pool-only frame");
    let surface_id = pool_id.to_string();
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("a freshly minted cpu-readback staging");

    let refusal = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect_err("a never-filled staging has no edit to publish");
    assert!(
        refusal.to_string().contains("never been read into"),
        "the refusal must name why, got: {refusal}"
    );

    // The frame is untouched — the refusal came before any submit.
    assert_every_byte_is(
        &read_pooled_backing_bytes(&pooled_backing),
        PUBLISHED_FRAME_STAMP,
        "the live frame after a refused write-back",
    );

    // And the guard lifts once the frame has actually been read in.
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("read the frame into the staging");
    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("a filled staging may publish its edit");
}

/// One staging spans every frame its pool slot publishes, so the
/// write-back must name the frame that was read in — not merely
/// *some* currently-valid frame.
///
/// The gap this closes is narrow, because `refuse_a_retired_frame_id`
/// already catches a write-back naming the *older* frame. What it
/// cannot catch is the other order: read `<slot>#N` in, let the slot
/// cycle, then publish against the now-current `<slot>#N+1` — every
/// earlier guard passes and frame N's pixels land on frame N+1's
/// backing. That is another frame's picture in the write direction,
/// which `[surface-id-lifetime-contract]` refuses.
/// GPU-gated: skips when no device is present.
#[test]
fn a_write_back_naming_a_different_frame_than_was_read_in_is_refused() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (first_pool_id, first_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the frame that gets read in");
    let read_in_surface_id = first_pool_id.to_string();
    let slot = crate::core::rhi::pool_slot_key_of_surface_id(&read_in_surface_id).to_string();
    stamp_every_byte_of_pooled_backing(&first_backing, PUBLISHED_FRAME_STAMP);

    let staging = gpu
        .surface_export_staging(
            &read_in_surface_id,
            SurfaceExportStagingResidency::HostVisible,
        )
        .expect("the cpu-readback staging");
    gpu.refill_surface_export_staging(&staging, &read_in_surface_id)
        .expect("read this frame in");

    // Let the slot cycle: release it, then take it back. The staging
    // is cached per slot, so the next generation gets this very one.
    drop(first_backing);
    let mut recycled = None;
    let mut _held_elsewhere = Vec::new();
    for _ in 0..8 {
        let (pool_id, backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("re-acquire from the pool");
        if crate::core::rhi::pool_slot_key_of_surface_id(&pool_id.to_string()) == slot {
            recycled = Some((pool_id.to_string(), backing));
            break;
        }
        _held_elsewhere.push(backing);
    }
    let Some((next_frame_surface_id, next_backing)) = recycled else {
        println!(
            "a_write_back_naming_a_different_frame_than_was_read_in_is_refused: the pool \
             never handed slot {slot} back — skipping"
        );
        return;
    };
    assert_ne!(
        next_frame_surface_id, read_in_surface_id,
        "recycling the slot must publish a new frame id"
    );
    stamp_every_byte_of_pooled_backing(&next_backing, TWO_FRAMES_LATER_STAMP);

    let refusal = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &next_frame_surface_id)
        .expect_err("a staging holding the previous frame must not publish over this one");
    assert!(
        refusal.to_string().contains(&read_in_surface_id),
        "the refusal must name the frame the staging actually holds, got: {refusal}"
    );
    assert_every_byte_is(
        &read_pooled_backing_bytes(&next_backing),
        TWO_FRAMES_LATER_STAMP,
        "the new frame's backing after a refused cross-frame write-back",
    );
}

/// What the staging holds and what it says it holds are written under
/// one lock, so they cannot be ordered against each other.
///
/// Two threads refilling one staging both take the recorder, and the
/// copies retire in the order the guard granted it. Recording the frame
/// id outside that guard let the two orders diverge: the staging would
/// hold one frame while the field named another, and a write-back for
/// the named frame would then pass the identity check and publish the
/// other frame's pixels. The field is written under the guard now, so
/// whichever copy submitted last is also the one the field names.
/// GPU-gated: skips when no device is present.
#[test]
fn concurrent_refills_leave_the_staging_naming_the_frame_it_actually_holds() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a frame to refill from");
    let surface_id = pool_id.to_string();
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("the cpu-readback staging");

    // Both threads refill the same staging, repeatedly, through the very
    // guard the bookkeeping now rides.
    let gpu = Arc::new(gpu);
    let both_are_ready = Arc::new(std::sync::Barrier::new(2));
    let refillers: Vec<_> = (0..2)
        .map(|_| {
            let gpu = Arc::clone(&gpu);
            let staging = Arc::clone(&staging);
            let surface_id = surface_id.clone();
            let both_are_ready = Arc::clone(&both_are_ready);
            std::thread::spawn(move || {
                both_are_ready.wait();
                for _ in 0..16 {
                    gpu.refill_surface_export_staging(&staging, &surface_id)
                        .expect("a refill of a live frame succeeds");
                }
            })
        })
        .collect();
    for refiller in refillers {
        refiller.join().expect("both refillers finish");
    }

    // Every refill named this frame, so the field must too — and the
    // write-back it gates must be accepted rather than refused as
    // holding somebody else's frame.
    assert_eq!(
        staging.frame_last_read_into_this_staging.lock().as_deref(),
        Some(surface_id.as_str()),
        "the staging must name the frame its last copy read in"
    );
    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("a staging naming this frame may publish back into it");
}

/// A staging's surface-share registration carries its refill timeline
/// exactly where the device exports one. On the software device it
/// registers the staging alone — creating the exportable timeline there
/// is refused by the driver, so minting one anyway would fail every
/// `cpu()` over a texture — and its copies answer with no value to wait
/// for.
/// GPU-gated: skips when no device is present.
#[test]
fn a_staging_registration_carries_a_timeline_edge_exactly_where_the_device_exports_one() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let device_exports_a_timeline = gpu.gpu_capabilities().supports_semaphore_export;
    let surface_share =
        crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a frame to export");
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);
    let surface_id = pool_id.to_string();

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("the staging mints on any device");
    let (shared_id, _) = gpu
        .share_surface_export_staging(&staging)
        .expect("the staging registers on any device");
    let registered = surface_share
        .state
        .get_surface_planes(&shared_id)
        .expect("the service holds the staging's registration");
    assert_eq!(
        registered.produce_done_fd.is_some(),
        device_exports_a_timeline,
        "the refill timeline crosses exactly where the device exports one"
    );
    assert!(
        registered.consume_done_fd.is_none(),
        "a staging never carries a consumer-side drain edge"
    );
    assert_eq!(
        staging
            .refill_done_timeline_another_process_waits_on()
            .is_some(),
        device_exports_a_timeline
    );

    let value_another_process_waits_for = gpu
        .refill_surface_export_staging(&staging, &surface_id)
        .expect("the refill runs on any device");
    assert_eq!(
        value_another_process_waits_for.is_some(),
        device_exports_a_timeline,
        "a refill answers a value to wait for only where a timeline crossed"
    );
    assert_every_byte_is(
        &read_cpu_readback_staging_through_its_mapping(&staging),
        PUBLISHED_FRAME_STAMP,
        "the refill, read off the mapping the moment it returns",
    );
    surface_share.stop();
}

/// The render-target probe's tiled DMA-BUF image, or `None` where this
/// device allocates one only LINEAR (the software device) or not at all
/// — where a display's staging cannot exist.
fn a_tiled_render_target_image_or_skip(gpu: &GpuContext) -> Option<Texture> {
    use crate::core::test_support::rig_only_capability_skip::{
        RigOnlyCapability, skip_because_this_machine_lacks_naming_the_refusal,
    };
    match gpu.acquire_render_target_dma_buf_image(
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
        TextureFormat::Rgba8Unorm,
    ) {
        Ok(image)
            if image
                .vulkan_inner()
                .is_a_dma_buf_image_with_a_tiled_drm_format_modifier() =>
        {
            Some(image)
        }
        Ok(_) => {
            skip_because_this_machine_lacks_naming_the_refusal(
                RigOnlyCapability::DmaBufModifiers,
                "this device allocates render targets only LINEAR".to_string(),
            );
            None
        }
        Err(unavailable) => {
            skip_because_this_machine_lacks_naming_the_refusal(
                RigOnlyCapability::DmaBufModifiers,
                format!("no DMA-BUF render target on this device: {unavailable}"),
            );
            None
        }
    }
}

/// A display's staging image read back through a host-visible copy.
fn read_display_importable_staging_into_host_bytes(
    gpu: &GpuContext,
    staging: &SurfaceExportStaging,
) -> Vec<u8> {
    let SurfaceExportStagingMemory::DisplayImportableImage(staging_image) = &staging.staging_memory
    else {
        panic!("a display's staging is an image");
    };
    let byte_size = staging.staging_byte_size();
    let host_readback = gpu
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                .expect("a non-zero staging"),
        )
        .expect("host-visible readback allocation");
    let mut recorder = gpu
        .create_command_recorder("display_importable_staging_test_readback")
        .expect("readback recorder");
    recorder.begin().expect("begin readback");
    recorder
        .record_image_read_as_transfer_source(staging_image, VulkanLayout::GENERAL, |recorder| {
            recorder.record_copy_image_to_buffer(
                staging_image,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                &host_readback,
                ImageCopyRegion::tightly_packed(staging.surface_width, staging.surface_height),
            )
        })
        .expect("copy the staging image into the readback allocation");
    recorder
        .record_buffer_barrier(
            &host_readback,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::HOST,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect("readback host-read barrier");
    recorder.submit_and_wait().expect("submit the readback");
    let mapped = host_readback.mapped_ptr();
    assert!(!mapped.is_null(), "the readback allocation must be mapped");
    unsafe { std::slice::from_raw_parts(mapped, byte_size as usize) }.to_vec()
}

/// A viewed texture frame in memory a display does not import (here a
/// device-local OPTIMAL texture, as an OPAQUE_FD pool slot is) is copied
/// on the GPU into a tiled DMA-BUF image registered with surface-share,
/// holding the frame's pixels. GPU-gated: skips where the device has no
/// tiled render target.
#[test]
fn a_viewed_texture_frame_is_copied_into_a_tiled_dma_buf_image_holding_its_pixels() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    if a_tiled_render_target_image_or_skip(&gpu).is_none() {
        return;
    }
    let surface_share =
        crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let surface_id = uuid::Uuid::new_v4().to_string();
    let viewed_texture = create_producer_owned_texture(&gpu);
    gpu.register_texture_with_layout(&surface_id, viewed_texture.clone(), VulkanLayout::UNDEFINED);
    let (_, upload_source) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the frame's upload source");
    stamp_every_byte_of_pooled_backing(&upload_source, KERNEL_OUTPUT_STAMP);
    gpu.copy_pixel_buffer_to_texture(
        &upload_source,
        &viewed_texture,
        &surface_id,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
    )
    .expect("render the viewed frame");

    let import_id = gpu
        .surface_share_id_a_viewer_reads_the_frame_through(
            &surface_id,
            ViewedFrameReadBy::DisplayImport,
            None,
        )
        .expect("the frame is copied for a display");
    assert_eq!(
        import_id,
        format!("{surface_id}-display-importable-staging")
    );
    let registered = surface_share
        .state
        .get_surface_planes(&import_id)
        .expect("the service holds the display staging's registration");
    assert_eq!(registered.handle_type, "dma_buf");
    assert_ne!(
        registered.drm_format_modifier, 0,
        "a display's staging carries a tiled modifier, never LINEAR"
    );
    let staging = gpu
        .surface_export_staging(
            &surface_id,
            SurfaceExportStagingResidency::DisplayImportable,
        )
        .expect("the display staging is cached");
    assert_every_byte_is(
        &read_display_importable_staging_into_host_bytes(&gpu, &staging),
        KERNEL_OUTPUT_STAMP,
        "the display's copy of a viewed texture frame",
    );
    surface_share.stop();
}

/// A viewer reading by CPU is answered the frame's host-visible staging,
/// registered as an OPAQUE_FD buffer that states the device it was
/// exported on and holding the frame's pixels, and a second ask copies
/// nothing more. GPU-gated: skips when no device is present.
#[test]
fn a_viewed_texture_frame_read_by_cpu_is_copied_into_a_cpu_readback_staging() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_share =
        crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let surface_id = uuid::Uuid::new_v4().to_string();
    let viewed_texture = create_producer_owned_texture(&gpu);
    gpu.register_texture_with_layout(&surface_id, viewed_texture.clone(), VulkanLayout::UNDEFINED);
    let (_, upload_source) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the frame's upload source");
    stamp_every_byte_of_pooled_backing(&upload_source, KERNEL_OUTPUT_STAMP);
    gpu.copy_pixel_buffer_to_texture(
        &upload_source,
        &viewed_texture,
        &surface_id,
        SURFACE_WIDTH,
        SURFACE_HEIGHT,
    )
    .expect("render the viewed frame");

    let import_id = gpu
        .surface_share_id_a_viewer_reads_the_frame_through(
            &surface_id,
            ViewedFrameReadBy::CpuRead,
            None,
        )
        .expect("the frame is copied for a CPU read");
    assert_eq!(import_id, format!("{surface_id}-cpu-readback-staging"));
    let registered = surface_share
        .state
        .get_surface_planes(&import_id)
        .expect("the service holds the CPU read staging's registration");
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::HostVisible)
        .expect("the CPU read staging is cached");
    assert_eq!(registered.handle_type, "opaque_fd");
    assert_eq!(
        registered.exporting_device_uuid,
        Some(streamlib_consumer_rhi::lowercase_hex_of_vulkan_device_uuid(
            staging.exporting_device_uuid()
        )),
        "a viewer imports the staging only on the device that exported it"
    );
    assert_every_byte_is(
        &read_cpu_readback_staging_through_its_mapping(&staging),
        KERNEL_OUTPUT_STAMP,
        "the CPU read copy of a viewed texture frame",
    );
    let signal_value_before_the_second_ask = staging.refill_submission.lock().next_signal_value;
    gpu.surface_share_id_a_viewer_reads_the_frame_through(
        &surface_id,
        ViewedFrameReadBy::CpuRead,
        None,
    )
    .expect("a second ask for the same frame");
    assert_eq!(
        staging.refill_submission.lock().next_signal_value,
        signal_value_before_the_second_ask,
        "a staging already holding the frame is not copied into again"
    );
    surface_share.stop();
}

/// A viewed pixel-buffer frame (LINEAR memory, which a display on NVIDIA
/// imports as black) is copied into the display's staging, and a second
/// ask for the same frame reuses that copy rather than copying again.
/// GPU-gated: skips where the device has no tiled render target.
#[test]
fn a_viewed_pixel_buffer_frame_is_copied_once_into_a_display_importable_image() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    if a_tiled_render_target_image_or_skip(&gpu).is_none() {
        return;
    }
    let surface_share =
        crate::core::test_support::LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the viewed frame");
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);
    let surface_id = pool_id.to_string();

    let import_id = gpu
        .surface_share_id_a_viewer_reads_the_frame_through(
            &surface_id,
            ViewedFrameReadBy::DisplayImport,
            None,
        )
        .expect("the frame is copied for a display");
    let staging = gpu
        .surface_export_staging(
            &surface_id,
            SurfaceExportStagingResidency::DisplayImportable,
        )
        .expect("the display staging is cached");
    assert_every_byte_is(
        &read_display_importable_staging_into_host_bytes(&gpu, &staging),
        PUBLISHED_FRAME_STAMP,
        "the display's copy of a viewed pixel-buffer frame",
    );

    stamp_every_byte_of_pooled_backing(&pooled_backing, TWO_FRAMES_LATER_STAMP);
    assert_eq!(
        gpu.surface_share_id_a_viewer_reads_the_frame_through(
            &surface_id,
            ViewedFrameReadBy::DisplayImport,
            None
        )
        .expect("the same frame is asked again"),
        import_id
    );
    assert_every_byte_is(
        &read_display_importable_staging_into_host_bytes(&gpu, &staging),
        PUBLISHED_FRAME_STAMP,
        "a second ask for a frame the staging already holds copies nothing",
    );
    surface_share.stop();
}

/// A frame already in a tiled DMA-BUF image crosses as its own slot, with
/// no copy and no staging.
/// GPU-gated: skips where the device has no tiled render target.
#[test]
fn a_viewed_frame_already_in_a_tiled_dma_buf_image_answers_its_own_slot() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let Some(render_target) = a_tiled_render_target_image_or_skip(&gpu) else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(&surface_id, render_target, VulkanLayout::GENERAL);

    assert_eq!(
        gpu.surface_share_id_a_viewer_reads_the_frame_through(
            &surface_id,
            ViewedFrameReadBy::DisplayImport,
            None
        )
        .expect("a display-importable frame is answered as it is"),
        surface_id
    );
    assert!(
        !gpu.surface_export_stagings
            .lock()
            .get(surface_id.as_str())
            .is_some_and(|by_residency| {
                by_residency.contains_key(&SurfaceExportStagingResidency::DisplayImportable)
            }),
        "no display staging is minted for a frame a display already imports"
    );
}

/// A display's staging is a viewer's copy: it is never a buffer to export
/// and never written back into the frame.
/// GPU-gated: skips where the device has no tiled render target.
#[test]
fn a_display_importable_staging_refuses_a_buffer_export_and_a_write_back() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    if a_tiled_render_target_image_or_skip(&gpu).is_none() {
        return;
    }
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the viewed frame");
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);
    let surface_id = pool_id.to_string();
    let staging = gpu
        .surface_export_staging(
            &surface_id,
            SurfaceExportStagingResidency::DisplayImportable,
        )
        .expect("a display staging mints");
    assert!(!staging.writable());
    assert!(staging.staging_buffer().is_err());
    assert!(
        gpu.export_surface_export_staging_opaque_fd(&staging)
            .is_err()
    );
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("the refill copies the frame");
    let refused = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect_err("a viewer's copy is never written back");
    assert!(
        refused.to_string().contains("nothing is written back"),
        "{refused}"
    );
}
