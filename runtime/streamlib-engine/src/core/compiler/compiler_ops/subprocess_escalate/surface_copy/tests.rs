// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `copy_surface_to_surface` driven through `handle_escalate_op` over a real
//! device: one test per backing pair, and one per refusal. GPU-gated: each
//! skips when no device is present.

use super::super::handle_escalate_op;
use super::super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestCopySurfaceToSurface;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::context::{GpuContext, GpuContextLimitedAccess, TextureRegistration};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, Texture, TextureDescriptor, TextureFormat, TextureUsages,
    VulkanLayout,
};
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;
use crate::host_rhi::{VulkanAccess, VulkanStage};
use crate::vulkan::rhi::ImageCopyRegion;

const PIXEL_WIDTH: u32 = 32;
const PIXEL_HEIGHT: u32 = 16;
const RGBA_BYTE_COUNT: usize = (PIXEL_WIDTH * PIXEL_HEIGHT * 4) as usize;

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::init_for_platform_sync() {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            println!("{test_name}: no GPU device — skipping");
            None
        }
    }
}

/// Bytes that differ pixel to pixel and row to row, so a copy that drops,
/// shears or repeats rows cannot match by accident.
fn a_distinct_byte_pattern(seed: u8) -> Vec<u8> {
    (0..RGBA_BYTE_COUNT)
        .map(|index| {
            (index as u32)
                .wrapping_mul(31)
                .wrapping_add(u32::from(seed)) as u8
        })
        .collect()
}

fn a_pool_frame_holding(
    gpu: &GpuContext,
    format: PixelFormat,
    pixels: &[u8],
) -> (String, PixelBuffer) {
    let (frame_id, pooled_backing) = gpu
        .acquire_pixel_buffer(PIXEL_WIDTH, PIXEL_HEIGHT, format)
        .expect("acquire a pool frame");
    pooled_backing
        .write_this_plane_from(0, pixels)
        .expect("fill the pool frame");
    (frame_id.to_string(), pooled_backing)
}

fn read_pool_frame(pooled_backing: &PixelBuffer) -> Vec<u8> {
    let base_address = pooled_backing.plane_base_address(0);
    assert!(!base_address.is_null(), "a pool frame is host-mapped");
    unsafe { std::slice::from_raw_parts(base_address, pooled_backing.plane_size(0) as usize) }
        .to_vec()
}

fn a_texture(gpu: &GpuContext, format: TextureFormat, usage: TextureUsages) -> Texture {
    gpu.device()
        .create_texture(
            &TextureDescriptor::new(PIXEL_WIDTH, PIXEL_HEIGHT, format).with_usage(usage),
        )
        .expect("create a texture")
}

fn every_copy_usage() -> TextureUsages {
    TextureUsages::COPY_SRC
        | TextureUsages::COPY_DST
        | TextureUsages::TEXTURE_BINDING
        | TextureUsages::STORAGE_BINDING
}

/// Register `texture` under a fresh surface id, first writing `pixels` into
/// it and leaving it in `resting_layout` when there are pixels to write.
fn register_texture_holding(
    gpu: &GpuContext,
    texture: Texture,
    pixels: Option<&[u8]>,
    resting_layout: VulkanLayout,
) -> String {
    let surface_id = uuid::Uuid::new_v4().to_string();
    let Some(pixels) = pixels else {
        gpu.register_texture_with_layout(&surface_id, texture, VulkanLayout::UNDEFINED);
        return surface_id;
    };
    let upload = gpu
        .acquire_storage_buffer(pixels.len() as u64)
        .expect("upload allocation");
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), upload.mapped_ptr(), pixels.len()) };
    let mut recorder = gpu
        .create_command_recorder("surface_copy_test_upload")
        .expect("recorder");
    recorder.begin().expect("begin");
    recorder
        .record_image_barrier(
            &texture,
            VulkanLayout::UNDEFINED,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            VulkanStage::HOST,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::HOST_WRITE,
            VulkanAccess::TRANSFER_WRITE,
        )
        .expect("to transfer-dst");
    recorder
        .record_copy_buffer_to_image(
            &upload,
            &texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            ImageCopyRegion::tightly_packed(PIXEL_WIDTH, PIXEL_HEIGHT),
        )
        .expect("upload");
    recorder
        .record_image_barrier(
            &texture,
            VulkanLayout::TRANSFER_DST_OPTIMAL,
            resting_layout,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::MEMORY_READ,
        )
        .expect("to resting layout");
    recorder.submit_and_wait().expect("submit the upload");
    gpu.register_texture_with_layout(&surface_id, texture, resting_layout);
    surface_id
}

fn registration_of(gpu: &GpuContext, surface_id: &str) -> TextureRegistration {
    gpu.producer_registered_texture_for_surface_id(surface_id)
        .expect("the texture is registered in this process")
}

/// The texture's pixels, read through a copy of this test's own — never the
/// copy under test — leaving it in the layout its registration names.
fn read_registered_texture(gpu: &GpuContext, surface_id: &str) -> Vec<u8> {
    let registration = registration_of(gpu, surface_id);
    let known_layout = registration.current_layout();
    let texture = registration.texture();
    let readback = gpu
        .acquire_storage_buffer(RGBA_BYTE_COUNT as u64)
        .expect("readback allocation");
    let mut recorder = gpu
        .create_command_recorder("surface_copy_test_readback")
        .expect("recorder");
    recorder.begin().expect("begin");
    recorder
        .record_image_barrier(
            texture,
            known_layout,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::ALL_TRANSFER,
            VulkanAccess::MEMORY_WRITE,
            VulkanAccess::TRANSFER_READ,
        )
        .expect("to transfer-src");
    recorder
        .record_copy_image_to_buffer(
            texture,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            &readback,
            ImageCopyRegion::tightly_packed(PIXEL_WIDTH, PIXEL_HEIGHT),
        )
        .expect("read back");
    recorder
        .record_image_barrier(
            texture,
            VulkanLayout::TRANSFER_SRC_OPTIMAL,
            known_layout,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::TRANSFER_READ,
            VulkanAccess::MEMORY_READ,
        )
        .expect("back to the known layout");
    recorder
        .record_buffer_barrier(
            &readback,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::HOST,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect("host-read barrier");
    recorder.submit_and_wait().expect("submit the readback");
    unsafe { std::slice::from_raw_parts(readback.mapped_ptr(), RGBA_BYTE_COUNT) }.to_vec()
}

/// Run the op the way a helper's request reaches it, answering the refusal
/// message on failure.
fn copy_through_the_escalate_op(
    gpu: &GpuContext,
    source_surface_id: &str,
    destination_surface_id: &str,
) -> Result<(), String> {
    let response = handle_escalate_op(
        &GpuContextLimitedAccess::new(gpu.clone()),
        &EscalateHandleRegistry::new(),
        &a_mesh_link_ingress_table_carrying_nothing(),
        EscalateRequest::CopySurfaceToSurface(EscalateRequestCopySurfaceToSurface {
            request_id: "copy-1".into(),
            source_surface_id: source_surface_id.into(),
            destination_surface_id: destination_surface_id.into(),
        }),
    )
    .expect("copy_surface_to_surface is request/response");
    match response {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "copy-1");
            Ok(())
        }
        EscalateResponse::Err(refusal) => {
            assert_eq!(refusal.request_id, "copy-1");
            Err(refusal.message)
        }
    }
}

fn assert_refused_saying(outcome: Result<(), String>, reason: &str, surface_named: &str) {
    let refusal = outcome.expect_err("the copy must be refused");
    assert!(
        refusal.contains(reason) && refusal.contains(surface_named),
        "the refusal must say {reason:?} and name {surface_named:?}, got: {refusal}"
    );
}

#[test]
fn a_pool_frame_copies_into_another_pool_frame() {
    let Some(gpu) = gpu_or_skip("a_pool_frame_copies_into_another_pool_frame") else {
        return;
    };
    let source_pixels = a_distinct_byte_pattern(3);
    let (source_id, _source) = a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &source_pixels);
    let (destination_id, destination) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(200));

    copy_through_the_escalate_op(&gpu, &source_id, &destination_id).expect("buffer→buffer copy");

    assert_eq!(read_pool_frame(&destination), source_pixels);
}

/// The pair a camera frame landing in a kernel's input texture is: the
/// frame's `rgba` and the texture's `rgba8_unorm` are one format.
#[test]
fn an_rgba_pool_frame_lands_in_an_rgba8_unorm_texture() {
    let Some(gpu) = gpu_or_skip("an_rgba_pool_frame_lands_in_an_rgba8_unorm_texture") else {
        return;
    };
    let source_pixels = a_distinct_byte_pattern(5);
    let (source_id, _source) = a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &source_pixels);
    let destination_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        None,
        VulkanLayout::UNDEFINED,
    );

    copy_through_the_escalate_op(&gpu, &source_id, &destination_id).expect("buffer→image copy");

    assert_eq!(
        registration_of(&gpu, &destination_id).current_layout(),
        VulkanLayout::GENERAL,
        "a texture nothing had written comes to rest in GENERAL and the registration says so"
    );
    assert_eq!(
        read_registered_texture(&gpu, &destination_id),
        source_pixels
    );
}

#[test]
fn a_texture_copies_into_a_pool_frame_and_keeps_its_layout() {
    let Some(gpu) = gpu_or_skip("a_texture_copies_into_a_pool_frame_and_keeps_its_layout") else {
        return;
    };
    let source_pixels = a_distinct_byte_pattern(9);
    let source_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        Some(&source_pixels),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
    );
    let (destination_id, destination) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(77));

    copy_through_the_escalate_op(&gpu, &source_id, &destination_id).expect("image→buffer copy");

    assert_eq!(read_pool_frame(&destination), source_pixels);
    assert_eq!(
        registration_of(&gpu, &source_id).current_layout(),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL
    );
}

/// The source comes back in the layout it was in, with its pixels intact —
/// a copy that barriered it from UNDEFINED would be free to discard them.
#[test]
fn a_texture_copies_into_a_texture_and_the_source_keeps_its_layout_and_pixels() {
    let Some(gpu) =
        gpu_or_skip("a_texture_copies_into_a_texture_and_the_source_keeps_its_layout_and_pixels")
    else {
        return;
    };
    let source_pixels = a_distinct_byte_pattern(11);
    let source_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        Some(&source_pixels),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
    );
    let destination_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        Some(&a_distinct_byte_pattern(99)),
        VulkanLayout::GENERAL,
    );

    copy_through_the_escalate_op(&gpu, &source_id, &destination_id).expect("image→image copy");

    assert_eq!(
        read_registered_texture(&gpu, &destination_id),
        source_pixels
    );
    assert_eq!(
        registration_of(&gpu, &destination_id).current_layout(),
        VulkanLayout::GENERAL
    );
    assert_eq!(
        registration_of(&gpu, &source_id).current_layout(),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL
    );
    assert_eq!(read_registered_texture(&gpu, &source_id), source_pixels);
}

#[test]
fn a_format_mismatch_is_refused_by_name() {
    let Some(gpu) = gpu_or_skip("a_format_mismatch_is_refused_by_name") else {
        return;
    };
    let (source_id, _source) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(1));
    let destination_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Bgra8Unorm, every_copy_usage()),
        None,
        VulkanLayout::UNDEFINED,
    );

    assert_refused_saying(
        copy_through_the_escalate_op(&gpu, &source_id, &destination_id),
        "format mismatch",
        &destination_id,
    );
}

/// Same bytes per pixel, different colour encoding: two textures compare
/// their own formats, not just the one-buffer pixel shape they share.
#[test]
fn a_unorm_texture_into_an_srgb_texture_is_refused_as_a_format_mismatch() {
    let Some(gpu) =
        gpu_or_skip("a_unorm_texture_into_an_srgb_texture_is_refused_as_a_format_mismatch")
    else {
        return;
    };
    let source_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        Some(&a_distinct_byte_pattern(2)),
        VulkanLayout::GENERAL,
    );
    let destination_id = register_texture_holding(
        &gpu,
        a_texture(
            &gpu,
            TextureFormat::Rgba8UnormSrgb,
            TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::TEXTURE_BINDING,
        ),
        None,
        VulkanLayout::UNDEFINED,
    );

    let refusal = copy_through_the_escalate_op(&gpu, &source_id, &destination_id)
        .expect_err("unorm into srgb must be refused");
    assert!(
        refusal.contains("format mismatch") && refusal.contains("rgba8_unorm_srgb"),
        "the refusal names both formats, got: {refusal}"
    );
}

#[test]
fn an_extent_mismatch_is_refused_by_name() {
    let Some(gpu) = gpu_or_skip("an_extent_mismatch_is_refused_by_name") else {
        return;
    };
    let (source_id, _source) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(1));
    let (destination_id, _destination) = gpu
        .acquire_pixel_buffer(PIXEL_WIDTH * 2, PIXEL_HEIGHT, PixelFormat::Rgba32)
        .expect("acquire a wider pool frame");
    let destination_id = destination_id.to_string();

    assert_refused_saying(
        copy_through_the_escalate_op(&gpu, &source_id, &destination_id),
        "extent mismatch",
        &destination_id,
    );
}

#[test]
fn a_retired_frame_is_refused_by_name() {
    let Some(gpu) = gpu_or_skip("a_retired_frame_is_refused_by_name") else {
        return;
    };
    let (source_id, _source) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(1));
    let (pool_slot, published_generation) =
        crate::core::rhi::split_pool_slot_and_frame_generation(&source_id)
            .expect("a pool frame id carries its generation");
    let retired_source_id = format!("{pool_slot}#{}", published_generation + 1);
    let (destination_id, destination) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(50));

    let refusal = copy_through_the_escalate_op(&gpu, &retired_source_id, &destination_id)
        .expect_err("a retired frame must be refused");
    assert!(
        refusal.contains(&retired_source_id),
        "the refusal names the retired frame, got: {refusal}"
    );
    assert_eq!(
        read_pool_frame(&destination),
        a_distinct_byte_pattern(50),
        "a refused copy writes nothing"
    );
}

#[test]
fn a_texture_without_copy_dst_is_refused_as_taking_no_write_back() {
    let Some(gpu) = gpu_or_skip("a_texture_without_copy_dst_is_refused_as_taking_no_write_back")
    else {
        return;
    };
    let (source_id, _source) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(1));
    let destination_id = register_texture_holding(
        &gpu,
        a_texture(
            &gpu,
            TextureFormat::Rgba8Unorm,
            TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING,
        ),
        None,
        VulkanLayout::UNDEFINED,
    );

    assert_refused_saying(
        copy_through_the_escalate_op(&gpu, &source_id, &destination_id),
        "cannot take a write-back",
        &destination_id,
    );
}

/// A pool frame a producer also published as a registered texture is still
/// that producer's, and nothing may be copied into it.
#[test]
fn a_pool_frame_its_producer_still_owns_is_refused_as_taking_no_write_back() {
    let Some(gpu) =
        gpu_or_skip("a_pool_frame_its_producer_still_owns_is_refused_as_taking_no_write_back")
    else {
        return;
    };
    let (source_id, _source) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(1));
    let destination_pixels = a_distinct_byte_pattern(60);
    let (destination_id, destination) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &destination_pixels);
    gpu.register_texture_with_layout(
        &destination_id,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        VulkanLayout::UNDEFINED,
    );

    assert_refused_saying(
        copy_through_the_escalate_op(&gpu, &source_id, &destination_id),
        "cannot take a write-back",
        &destination_id,
    );
    assert_eq!(read_pool_frame(&destination), destination_pixels);
}

#[test]
fn a_texture_nothing_has_written_is_refused_as_a_source() {
    let Some(gpu) = gpu_or_skip("a_texture_nothing_has_written_is_refused_as_a_source") else {
        return;
    };
    let source_id = register_texture_holding(
        &gpu,
        a_texture(&gpu, TextureFormat::Rgba8Unorm, every_copy_usage()),
        None,
        VulkanLayout::UNDEFINED,
    );
    let (destination_id, _destination) =
        a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &a_distinct_byte_pattern(1));

    assert_refused_saying(
        copy_through_the_escalate_op(&gpu, &source_id, &destination_id),
        "never been written",
        &source_id,
    );
}

/// A copy onto its own source would overlap it, so it is refused rather
/// than recorded.
#[test]
fn a_frame_copied_onto_itself_is_refused_as_one_allocation() {
    let Some(gpu) = gpu_or_skip("a_frame_copied_onto_itself_is_refused_as_one_allocation") else {
        return;
    };
    let frame_pixels = a_distinct_byte_pattern(1);
    let (frame_id, frame) = a_pool_frame_holding(&gpu, PixelFormat::Rgba32, &frame_pixels);

    assert_refused_saying(
        copy_through_the_escalate_op(&gpu, &frame_id, &frame_id),
        "one allocation",
        &frame_id,
    );
    assert_eq!(read_pool_frame(&frame), frame_pixels);
}
