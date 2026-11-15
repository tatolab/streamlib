// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Device exports reading the published frame, and write-backs into pooled backings and textures with their refusals.

use super::*;

/// Ground truth, not view-identity: once the producer has come back
/// around to the ring slot, a device export refilled for the bag's
/// surface id still carries the pixels that bag was published with.
///
/// Mental-revert: resolving the export texture-first blits the ring
/// slot the producer has already overwritten, and this reads
/// `TWO_FRAMES_LATER_STAMP`: the stale-ring-slot read, reproduced at engine level.
/// GPU-gated: skips when no device is present.
#[test]
fn a_device_export_reads_the_published_frame_after_the_producer_ring_advances() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let published = RotatingProducerPublishedFrame::publish(&gpu, PUBLISHED_FRAME_STAMP);

    // The consumer takes its export at bag receipt; the producer then
    // runs ahead of it.
    let staging = gpu
        .surface_export_staging(
            &published.surface_id,
            SurfaceExportStagingResidency::DeviceLocal,
        )
        .expect("device-export staging for the published frame");
    published.advance_ring_texture_to(&gpu, TWO_FRAMES_LATER_STAMP);

    gpu.refill_surface_export_staging(&staging, &published.surface_id)
        .expect("refill the device export");
    assert_every_byte_is(
        &read_device_export_staging_into_host_bytes(&gpu, &staging),
        PUBLISHED_FRAME_STAMP,
        "the device export of a frame the producer has run past",
    );
}

/// A frame the producer also published as a registered texture is one
/// the producer still owns: an in-place device edit would land in a
/// live pool slot, so the export is read-only.
/// GPU-gated: skips when no device is present.
#[test]
fn a_dual_backed_export_refuses_an_in_place_edit() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let published = RotatingProducerPublishedFrame::publish(&gpu, PUBLISHED_FRAME_STAMP);
    let staging = gpu
        .surface_export_staging(
            &published.surface_id,
            SurfaceExportStagingResidency::DeviceLocal,
        )
        .expect("device-export staging for the published frame");

    assert!(
        !staging.writable(),
        "a surface backed by both a pool member and a producer's registered texture \
         must export read-only"
    );
    let refusal = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &published.surface_id)
        .expect_err("a dual-backed export must refuse a write-back");
    assert!(
        refusal.to_string().contains("read-only"),
        "the refusal must say the export is read-only, got: {refusal}"
    );
}

/// A surface whose only backing is its pool member keeps the
/// write-back protocol — the shape every green wheel device-edit test
/// exercises. GPU-gated: skips when no device is present.
#[test]
fn a_pool_only_export_stays_writable_and_publishes_its_edit() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the frame's pooled backing");
    let surface_id = pool_id.to_string();
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the published frame");
    assert!(
        staging.writable(),
        "a surface whose only backing is its pool member keeps the write-back path"
    );

    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("refill the device export");
    assert_every_byte_is(
        &read_device_export_staging_into_host_bytes(&gpu, &staging),
        PUBLISHED_FRAME_STAMP,
        "the device export of a pool-only surface",
    );

    stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("publish the device-side edit back to the surface");

    assert_every_byte_is(
        &read_pooled_backing_bytes(&pooled_backing),
        CONSUMER_EDIT_STAMP,
        "the surface's own allocation after the edit was published",
    );
}

/// The advertised capability is a snapshot; the write-back is not.
///
/// A pool slot keeps its id across reuse, so a surface that was
/// pool-only when a consumer opened its export read-write can be
/// handed back out to a texture-registering producer while that
/// consumer still holds the staging. The export keeps saying
/// writable — it is what the consumer was told — and the write-back
/// refuses anyway.
///
/// Mental-revert: gate the write-back on `staging.writable` alone
/// and the staged edit lands in the new owner's live pool slot.
/// GPU-gated: skips when no device is present.
#[test]
fn a_write_back_refuses_once_a_producer_registers_a_texture_over_the_slot() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let (pool_id, pooled_backing) = gpu
        .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire the frame's pooled backing");
    let surface_id = pool_id.to_string();
    stamp_every_byte_of_pooled_backing(&pooled_backing, PUBLISHED_FRAME_STAMP);

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the published frame");
    assert!(
        staging.writable(),
        "the surface was pool-only when the export was opened"
    );
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("refill the device export");
    stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);

    gpu.register_texture_with_layout(
        &surface_id,
        create_producer_owned_texture(&gpu),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
    );

    let refusal = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect_err("the write-back must refuse a slot a producer has taken over");
    assert!(
        refusal.to_string().contains("registered texture"),
        "the refusal must name the registration that took the slot, got: {refusal}"
    );
    assert_every_byte_is(
        &read_pooled_backing_bytes(&pooled_backing),
        PUBLISHED_FRAME_STAMP,
        "the pooled allocation after the refused write-back",
    );
}

/// Kernel outputs have no pooled member, so the registered texture
/// stays the export's source — writable, because the pool-member
/// rule has nothing to protect there. GPU-gated: skips when no
/// device is present.
#[test]
fn a_surface_with_no_pooled_backing_still_exports_its_registered_texture() {
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
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the kernel output");
    assert!(
        staging.writable(),
        "a texture-backed export is writable: a kernel output has no pooled member, so \
         the pool-member rule has nothing to protect"
    );

    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("refill the device export");
    assert_every_byte_is(
        &read_device_export_staging_into_host_bytes(&gpu, &staging),
        KERNEL_OUTPUT_STAMP,
        "the device export of a surface with no pooled backing",
    );
}

/// The scope's write direction: an edit staged over a texture-backed
/// export publishes back into the registered texture itself, proven
/// by a second refill reading the edit out of the texture.
/// GPU-gated: skips when no device is present.
#[test]
fn a_texture_backed_export_publishes_its_edit_back_into_the_texture() {
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
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the kernel output");
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("read the kernel output into the staging");
    stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("publish the edit into the texture");

    // Round trip: a fresh refill reads the texture, not the staging's
    // leftover bytes, so the edit surviving it proves the texture took
    // the write.
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("re-read the texture after the write-back");
    assert_every_byte_is(
        &read_device_export_staging_into_host_bytes(&gpu, &staging),
        CONSUMER_EDIT_STAMP,
        "the texture's pixels after a staged edit published back",
    );
}

/// The read-before-write rule holds for the texture arm too: a
/// staging never refilled has nothing to edit, and publishing it
/// would write allocator garbage over the kernel's output.
/// GPU-gated: skips when no device is present.
#[test]
fn a_texture_backed_write_back_of_a_never_filled_staging_is_refused() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(
        &surface_id,
        create_producer_owned_texture(&gpu),
        VulkanLayout::UNDEFINED,
    );
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the kernel output");
    let refusal = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect_err("a never-filled texture staging must not publish");
    assert!(
        refusal.to_string().contains("never been read into"),
        "the refusal must name the missing read, got: {refusal}"
    );
}

/// The upload direction: a staging written in full replaces every pixel
/// of a texture nothing was ever read out of, and afterwards names that
/// frame as the one it holds. GPU-gated: skips when no device is present.
#[test]
fn a_whole_staging_upload_replaces_every_pixel_of_a_never_read_texture() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(
        &surface_id,
        create_producer_owned_texture(&gpu),
        VulkanLayout::UNDEFINED,
    );
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the upload target");
    stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
    gpu.copy_surface_export_staging_over_every_pixel_of_surface(&staging, &surface_id)
        .expect("an upload needs no read-in");
    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("after an upload the staging holds the frame it wrote");

    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("re-read the texture after the upload");
    assert_every_byte_is(
        &read_device_export_staging_into_host_bytes(&gpu, &staging),
        CONSUMER_EDIT_STAMP,
        "the texture's pixels after a whole-staging upload",
    );
}

/// The float path's write direction: an rgba16_float edit staged at
/// 8 bytes per pixel publishes into the texture and survives a fresh
/// refill — a stride or region error in the wider-than-4-byte
/// arithmetic would corrupt this round trip, not the mint the sizing
/// test covers. GPU-gated: skips when no device is present.
#[test]
fn a_float_format_edit_round_trips_through_the_texture() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    let descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba16Float)
            .with_usage(
                TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::STORAGE_BINDING,
            );
    let float_texture = gpu
        .device()
        .create_texture(&descriptor)
        .expect("rgba16_float kernel output texture");
    gpu.register_texture_with_layout(&surface_id, float_texture, VulkanLayout::UNDEFINED);
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("a float texture's export staging");

    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("read the (undefined) float texture in");
    stamp_every_byte_of_device_export_staging(&gpu, &staging, CONSUMER_EDIT_STAMP);
    gpu.copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect("publish the float edit into the texture");
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("re-read the texture after the write-back");
    assert_every_byte_is(
        &read_device_export_staging_into_host_bytes(&gpu, &staging),
        CONSUMER_EDIT_STAMP,
        "an rgba16_float texture's pixels after a staged edit published back",
    );
}

/// A rotating producer re-registering the slot with a different
/// format or geometry is judged at the copy, not trusted from mint:
/// a same-size format swap would relabel channels, and a new extent
/// would mis-size the copy region. GPU-gated: skips when no device
/// is present.
#[test]
fn a_re_registration_with_a_different_shape_is_refused_at_the_copy() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(
        &surface_id,
        create_producer_owned_texture(&gpu),
        VulkanLayout::UNDEFINED,
    );
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the rgba8 registration");

    // Same byte size, different channel order — the case a
    // size-only guard would silently relabel.
    let bgra_descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Bgra8Unorm)
            .with_usage(TextureUsages::COPY_SRC | TextureUsages::COPY_DST);
    gpu.register_texture_with_layout(
        &surface_id,
        gpu.device()
            .create_texture(&bgra_descriptor)
            .expect("the re-registered bgra texture"),
        VulkanLayout::UNDEFINED,
    );
    let format_refusal = gpu
        .refill_surface_export_staging(&staging, &surface_id)
        .expect_err("a same-size format swap must refuse");
    assert!(
        format_refusal.to_string().contains("re-registered as"),
        "the refusal must name the format change, got: {format_refusal}"
    );

    // Same format, different extent — the copy region would misfit.
    let smaller_descriptor = TextureDescriptor::new(
        SURFACE_WIDTH / 2,
        SURFACE_HEIGHT / 2,
        TextureFormat::Rgba8Unorm,
    )
    .with_usage(TextureUsages::COPY_SRC | TextureUsages::COPY_DST);
    gpu.register_texture_with_layout(
        &surface_id,
        gpu.device()
            .create_texture(&smaller_descriptor)
            .expect("the re-registered smaller texture"),
        VulkanLayout::UNDEFINED,
    );
    let geometry_refusal = gpu
        .refill_surface_export_staging(&staging, &surface_id)
        .expect_err("a geometry change must refuse");
    assert!(
        geometry_refusal.to_string().contains("different geometry"),
        "the refusal must name the geometry change, got: {geometry_refusal}"
    );
}

/// Recording a copy against an image whose usage forbids it is a
/// Vulkan spec violation the driver never reports — the refill
/// refuses by name instead. GPU-gated: skips when no device is
/// present.
#[test]
fn a_refill_of_a_texture_without_copy_src_usage_is_refused() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    let descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING);
    let sampled_only_texture = gpu
        .device()
        .create_texture(&descriptor)
        .expect("sampled-only texture");
    gpu.register_texture_with_layout(&surface_id, sampled_only_texture, VulkanLayout::UNDEFINED);
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("the staging mints from format and geometry alone");
    let refusal = gpu
        .refill_surface_export_staging(&staging, &surface_id)
        .expect_err("a copy out of a sampled-only image must refuse");
    assert!(
        refusal.to_string().contains("copy_src"),
        "the refusal must name the missing usage, got: {refusal}"
    );
}

/// The write direction's twin: an image without COPY_DST advertises
/// a read-only export and refuses a publish by naming the usage.
/// GPU-gated: skips when no device is present.
#[test]
fn a_write_back_into_a_texture_without_copy_dst_usage_is_refused() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    let descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm)
            .with_usage(TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC);
    let readable_only_texture = gpu
        .device()
        .create_texture(&descriptor)
        .expect("readable-only texture");
    gpu.register_texture_with_layout(&surface_id, readable_only_texture, VulkanLayout::UNDEFINED);
    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("device-export staging for the readable-only texture");
    assert!(
        !staging.writable(),
        "an image that cannot take a recorded copy must advertise a read-only export"
    );
    gpu.refill_surface_export_staging(&staging, &surface_id)
        .expect("the read direction needs only copy_src");
    let refusal = gpu
        .copy_surface_export_staging_back_to_surface(&staging, &surface_id)
        .expect_err("a copy into a copy_dst-less image must refuse");
    assert!(
        refusal.to_string().contains("copy_dst"),
        "the refusal must name the missing usage, got: {refusal}"
    );
}

/// A float kernel output sizes its staging by its own pixel width
/// and presents its float identity — the HDR shape the scope exists
/// for. GPU-gated: skips when no device is present.
#[test]
fn a_float_format_texture_sizes_its_staging_by_its_own_pixel_width() {
    let Some(gpu) = gpu_context_or_skip() else {
        return;
    };
    let surface_id = uuid::Uuid::new_v4().to_string();
    let descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba16Float)
            .with_usage(
                TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::STORAGE_BINDING,
            );
    let float_texture = gpu
        .device()
        .create_texture(&descriptor)
        .expect("rgba16_float kernel output texture");
    gpu.register_texture_with_layout(&surface_id, float_texture, VulkanLayout::UNDEFINED);

    let staging = gpu
        .surface_export_staging(&surface_id, SurfaceExportStagingResidency::DeviceLocal)
        .expect("a float texture's export staging");
    assert_eq!(
        staging.staging_byte_size(),
        u64::from(SURFACE_WIDTH) * u64::from(SURFACE_HEIGHT) * 8,
        "an rgba16_float staging spans 8 bytes per pixel"
    );
    assert_eq!(
        staging.pixel_format(),
        Some(PixelFormat::Rgba16Float),
        "the staged shape keeps its float identity"
    );
    assert!(staging.writable(), "a texture-backed export is writable");
}
