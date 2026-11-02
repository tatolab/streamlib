// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `convert_surface_into` driven through `handle_escalate_op` over a real
//! device: a pooled YUYV frame, an adopted allocation whose registration
//! states a padded plane layout, and each refusal. GPU-gated: each skips when
//! no device is present.

use super::super::handle_escalate_op;
use super::super::handle_lifecycle::EscalateHandleRegistry;
use crate::core::compiler::compiler_ops::subprocess_escalate::CatalogEntriesAHelperPlacedProcessorReaches;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::{
    EscalateRequestColorInfoAxisNames, EscalateRequestConvertSurfaceInto,
    EscalateRequestReleaseHandle,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::{
    EscalateRequest, EscalateResponse,
};
use crate::core::context::{GpuContext, GpuContextLimitedAccess};
use crate::core::rhi::{
    PixelFormat, PixelPlanesWithinOneAllocation, TextureDescriptor, TextureFormat, TextureUsages,
    VulkanLayout,
};
use crate::core::test_support::LiveSurfaceShareServiceForATest;
use crate::host_rhi::{VulkanAccess, VulkanStage};
use crate::vulkan::rhi::ImageCopyRegion;

const PIXEL_WIDTH: u32 = 32;
const PIXEL_HEIGHT: u32 = 16;

/// Limited-range luma at the top and bottom of the BT.709 scale.
const LIMITED_RANGE_WHITE_LUMA: u8 = 235;
const LIMITED_RANGE_BLACK_LUMA: u8 = 16;
const NEUTRAL_CHROMA: u8 = 128;

/// What padding bytes hold, chosen so a read that lands in padding converts to
/// a colour neither white nor black.
const PADDING_BYTE: u8 = 0x5a;

/// The tolerance for one 8-bit channel after a float conversion.
const CHANNEL_TOLERANCE: u8 = 2;

fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
    match GpuContext::init_for_platform_sync() {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            println!("{test_name}: no GPU device — skipping");
            None
        }
    }
}

fn bt709_limited() -> EscalateRequestColorInfoAxisNames {
    EscalateRequestColorInfoAxisNames {
        primaries: Some("bt709".into()),
        transfer: Some("bt709".into()),
        matrix: Some("bt709".into()),
        range: Some("limited".into()),
    }
}

/// Luma white in the left half of each row and black in the right, so a read
/// that shears rows or ignores the row pitch cannot match.
fn luma_of_column(column: u32) -> u8 {
    if column < PIXEL_WIDTH / 2 {
        LIMITED_RANGE_WHITE_LUMA
    } else {
        LIMITED_RANGE_BLACK_LUMA
    }
}

/// NV12 bytes for the test frame at `luma_row_pitch`, with the chroma plane at
/// `chroma_offset` and every byte outside the pixels set to padding.
fn nv12_bytes(allocation_byte_size: usize, luma_row_pitch: usize, chroma_offset: usize) -> Vec<u8> {
    let mut bytes = vec![PADDING_BYTE; allocation_byte_size];
    for row in 0..PIXEL_HEIGHT as usize {
        for column in 0..PIXEL_WIDTH {
            bytes[row * luma_row_pitch + column as usize] = luma_of_column(column);
        }
    }
    for chroma_row in 0..(PIXEL_HEIGHT / 2) as usize {
        let start = chroma_offset + chroma_row * luma_row_pitch;
        bytes[start..start + PIXEL_WIDTH as usize].fill(NEUTRAL_CHROMA);
    }
    bytes
}

fn a_storage_texture(gpu: &GpuContext, width: u32, usage: TextureUsages) -> String {
    let texture = gpu
        .device()
        .create_texture(
            &TextureDescriptor::new(width, PIXEL_HEIGHT, TextureFormat::Rgba8Unorm)
                .with_usage(usage),
        )
        .expect("create a texture");
    let surface_id = uuid::Uuid::new_v4().to_string();
    gpu.register_texture_with_layout(&surface_id, texture, VulkanLayout::UNDEFINED);
    surface_id
}

fn storage_and_copy_usage() -> TextureUsages {
    TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC | TextureUsages::TEXTURE_BINDING
}

/// The texture's RGBA pixels, read with a copy of this test's own.
fn read_registered_texture(gpu: &GpuContext, surface_id: &str) -> Vec<u8> {
    let registration = gpu
        .producer_registered_texture_for_surface_id(surface_id)
        .expect("the texture is registered in this process");
    let known_layout = registration.current_layout();
    let texture = registration.texture();
    let byte_count = (PIXEL_WIDTH * PIXEL_HEIGHT * 4) as usize;
    let readback = gpu
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_count as u64)
                .expect("a non-zero readback"),
        )
        .expect("readback allocation");
    let mut recorder = gpu
        .create_command_recorder("surface_color_conversion_test_readback")
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
        .record_buffer_barrier(
            &readback,
            VulkanStage::ALL_TRANSFER,
            VulkanStage::HOST,
            VulkanAccess::TRANSFER_WRITE,
            VulkanAccess::HOST_READ,
        )
        .expect("host-read barrier");
    recorder.submit_and_wait().expect("submit the readback");
    unsafe { std::slice::from_raw_parts(readback.mapped_ptr(), byte_count) }.to_vec()
}

fn assert_white_left_and_black_right(rgba: &[u8]) {
    for row in 0..PIXEL_HEIGHT {
        for column in 0..PIXEL_WIDTH {
            let at = ((row * PIXEL_WIDTH + column) * 4) as usize;
            let expected = if luma_of_column(column) == LIMITED_RANGE_WHITE_LUMA {
                255
            } else {
                0
            };
            for channel in 0..3 {
                let value = rgba[at + channel];
                assert!(
                    value.abs_diff(expected) <= CHANNEL_TOLERANCE,
                    "pixel ({column}, {row}) channel {channel} is {value}, expected {expected}"
                );
            }
            assert_eq!(rgba[at + 3], 255, "pixel ({column}, {row}) is opaque");
        }
    }
}

fn convert_through_the_escalate_op(
    gpu: &GpuContext,
    registry: &EscalateHandleRegistry,
    source_surface_id: &str,
    destination_surface_id: &str,
    source_color: EscalateRequestColorInfoAxisNames,
) -> Result<(), String> {
    let response = handle_escalate_op(
        &GpuContextLimitedAccess::new(gpu.clone()),
        registry,
        &CatalogEntriesAHelperPlacedProcessorReaches::default(),
        EscalateRequest::ConvertSurfaceInto(EscalateRequestConvertSurfaceInto {
            request_id: "convert-1".into(),
            source_surface_id: source_surface_id.into(),
            destination_surface_id: destination_surface_id.into(),
            source_color,
        }),
    )
    .expect("convert_surface_into is request/response");
    match response {
        EscalateResponse::Ok(ok) => {
            assert_eq!(ok.request_id, "convert-1");
            Ok(())
        }
        EscalateResponse::Err(refusal) => {
            assert_eq!(refusal.request_id, "convert-1");
            Err(refusal.message)
        }
    }
}

/// Tight YUYV bytes for the test frame: each pixel pair is `Y0 Cb Y1 Cr`.
fn yuyv_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity((PIXEL_WIDTH * PIXEL_HEIGHT * 2) as usize);
    for _row in 0..PIXEL_HEIGHT {
        for pair_start in (0..PIXEL_WIDTH).step_by(2) {
            bytes.extend_from_slice(&[
                luma_of_column(pair_start),
                NEUTRAL_CHROMA,
                luma_of_column(pair_start + 1),
                NEUTRAL_CHROMA,
            ]);
        }
    }
    bytes
}

fn a_pooled_yuyv_frame(gpu: &GpuContext) -> (String, crate::core::rhi::PixelBuffer) {
    let (frame_id, pooled) = gpu
        .acquire_pixel_buffer(PIXEL_WIDTH, PIXEL_HEIGHT, PixelFormat::Yuyv422)
        .expect("acquire a pooled YUYV frame");
    pooled
        .write_this_plane_from(0, &yuyv_bytes())
        .expect("fill the pooled frame");
    (frame_id.to_string(), pooled)
}

#[test]
fn a_pooled_yuyv_frame_converts_into_an_rgba_texture() {
    let Some(gpu) = gpu_or_skip("a_pooled_yuyv_frame_converts_into_an_rgba_texture") else {
        return;
    };
    let (source_id, _pooled) = a_pooled_yuyv_frame(&gpu);
    let destination_id = a_storage_texture(&gpu, PIXEL_WIDTH, storage_and_copy_usage());
    let registry = EscalateHandleRegistry::new();

    convert_through_the_escalate_op(
        &gpu,
        &registry,
        &source_id,
        &destination_id,
        bt709_limited(),
    )
    .expect("YUYV → RGBA");
    assert!(
        !registry
            .surface_color_conversion_session()
            .keeps_a_source_for(&source_id),
        "a pooled frame is resolved per conversion, so its slot goes back to the pool"
    );

    assert_white_left_and_black_right(&read_registered_texture(&gpu, &destination_id));
    assert_eq!(
        gpu.producer_registered_texture_for_surface_id(&destination_id)
            .expect("registered")
            .current_layout(),
        VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        "the destination rests where a sampler reads it"
    );
}

#[test]
fn an_adopted_allocation_converts_through_the_padded_layout_its_registration_states() {
    let Some(gpu) = gpu_or_skip(
        "an_adopted_allocation_converts_through_the_padded_layout_its_registration_states",
    ) else {
        return;
    };
    let service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let luma_row_pitch = 2 * PIXEL_WIDTH as usize;
    let chroma_offset = luma_row_pitch * PIXEL_HEIGHT as usize + 64;
    let (_, allocation) = gpu
        .acquire_pixel_buffer(PIXEL_WIDTH, 2 * PIXEL_HEIGHT, PixelFormat::Rgba32)
        .expect("an allocation to adopt");
    let allocation_byte_size = allocation.plane_size(0) as usize;
    allocation
        .write_this_plane_from(
            0,
            &nv12_bytes(allocation_byte_size, luma_row_pitch, chroma_offset),
        )
        .expect("fill the allocation");
    let planes = PixelPlanesWithinOneAllocation::new(
        PixelFormat::Nv12VideoRange,
        PIXEL_WIDTH,
        PIXEL_HEIGHT,
        allocation_byte_size as u64,
        vec![0, chroma_offset as u64],
        vec![luma_row_pitch as u64; 2],
        true,
    )
    .expect("the padded layout fits the allocation");
    let source_id = gpu
        .surface_store()
        .expect("the service's store is installed")
        .check_in_with_a_pixel_plane_layout(
            &allocation,
            PixelFormat::Nv12VideoRange,
            PIXEL_WIDTH,
            PIXEL_HEIGHT,
            &planes,
        )
        .expect("the service adopts the allocation with its layout");
    let destination_id = a_storage_texture(&gpu, PIXEL_WIDTH, storage_and_copy_usage());
    let registry = EscalateHandleRegistry::new();

    convert_through_the_escalate_op(
        &gpu,
        &registry,
        &source_id,
        &destination_id,
        bt709_limited(),
    )
    .expect("the adopted NV12 converts");
    assert_white_left_and_black_right(&read_registered_texture(&gpu, &destination_id));

    convert_through_the_escalate_op(
        &gpu,
        &registry,
        &source_id,
        &destination_id,
        bt709_limited(),
    )
    .expect("a second conversion reads the source this helper already resolved");
    assert_white_left_and_black_right(&read_registered_texture(&gpu, &destination_id));

    let released = handle_escalate_op(
        &GpuContextLimitedAccess::new(gpu.clone()),
        &registry,
        &CatalogEntriesAHelperPlacedProcessorReaches::default(),
        EscalateRequest::ReleaseHandle(EscalateRequestReleaseHandle {
            request_id: "release-1".into(),
            handle_id: source_id.clone(),
        }),
    )
    .expect("release_handle is request/response");
    assert!(
        matches!(released, EscalateResponse::Ok(_)),
        "releasing a conversion source the helper resolved is answered: {released:?}"
    );
    service.stop();
}

#[test]
fn a_registration_whose_layout_runs_past_its_allocation_is_refused() {
    let Some(gpu) = gpu_or_skip("a_registration_whose_layout_runs_past_its_allocation_is_refused")
    else {
        return;
    };
    let service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let (_, allocation) = gpu
        .acquire_pixel_buffer(PIXEL_WIDTH, PIXEL_HEIGHT / 2, PixelFormat::Rgba32)
        .expect("a small allocation");
    let overreaching = PixelPlanesWithinOneAllocation::tightly_packed(
        PixelFormat::Nv12VideoRange,
        PIXEL_WIDTH,
        8 * PIXEL_HEIGHT,
    )
    .expect("tight NV12 lays out");
    let refusal = gpu
        .surface_store()
        .expect("the service's store is installed")
        .check_in_with_a_pixel_plane_layout(
            &allocation,
            PixelFormat::Nv12VideoRange,
            PIXEL_WIDTH,
            8 * PIXEL_HEIGHT,
            &overreaching,
        )
        .expect_err("the service refuses a layout past the allocation");
    assert!(refusal.to_string().contains("runs past"), "{refusal}");
    service.stop();
}

#[test]
fn a_destination_without_storage_usage_is_refused_naming_it() {
    let Some(gpu) = gpu_or_skip("a_destination_without_storage_usage_is_refused_naming_it") else {
        return;
    };
    let (source_id, _pooled) = a_pooled_yuyv_frame(&gpu);
    let destination_id = a_storage_texture(
        &gpu,
        PIXEL_WIDTH,
        TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_SRC,
    );
    let refusal = convert_through_the_escalate_op(
        &gpu,
        &EscalateHandleRegistry::new(),
        &source_id,
        &destination_id,
        bt709_limited(),
    )
    .expect_err("a texture the converter cannot write is refused");
    assert!(
        refusal.contains("storage_binding") && refusal.contains(&destination_id),
        "{refusal}"
    );
}

#[test]
fn a_destination_of_another_extent_is_refused() {
    let Some(gpu) = gpu_or_skip("a_destination_of_another_extent_is_refused") else {
        return;
    };
    let (source_id, _pooled) = a_pooled_yuyv_frame(&gpu);
    let destination_id = a_storage_texture(&gpu, PIXEL_WIDTH * 2, storage_and_copy_usage());
    let refusal = convert_through_the_escalate_op(
        &gpu,
        &EscalateHandleRegistry::new(),
        &source_id,
        &destination_id,
        bt709_limited(),
    )
    .expect_err("the conversion scales nothing");
    assert!(refusal.contains("scales nothing"), "{refusal}");
}

#[test]
fn an_rgba_source_is_refused_naming_the_formats_the_converter_reads() {
    let Some(gpu) = gpu_or_skip("an_rgba_source_is_refused_naming_the_formats_the_converter_reads")
    else {
        return;
    };
    let (source_id, _pooled) = gpu
        .acquire_pixel_buffer(PIXEL_WIDTH, PIXEL_HEIGHT, PixelFormat::Rgba32)
        .expect("an RGBA frame");
    let destination_id = a_storage_texture(&gpu, PIXEL_WIDTH, storage_and_copy_usage());
    let refusal = convert_through_the_escalate_op(
        &gpu,
        &EscalateHandleRegistry::new(),
        &source_id.to_string(),
        &destination_id,
        bt709_limited(),
    )
    .expect_err("RGBA is not a YCbCr source");
    assert!(refusal.contains("nv12_video_range"), "{refusal}");
}

#[test]
fn an_unknown_color_name_is_refused_before_any_gpu_work() {
    let Some(gpu) = gpu_or_skip("an_unknown_color_name_is_refused_before_any_gpu_work") else {
        return;
    };
    let (source_id, _pooled) = a_pooled_yuyv_frame(&gpu);
    let destination_id = a_storage_texture(&gpu, PIXEL_WIDTH, storage_and_copy_usage());
    let refusal = convert_through_the_escalate_op(
        &gpu,
        &EscalateHandleRegistry::new(),
        &source_id,
        &destination_id,
        EscalateRequestColorInfoAxisNames {
            range: Some("tv".into()),
            ..bt709_limited()
        },
    )
    .expect_err("tv is not a range name");
    assert!(refusal.contains("\"tv\""), "{refusal}");
}

/// A memfd holding `bytes`, standing in for a foreign device's DMA-BUF: the
/// host mapping reaches it through the same `mmap`.
#[cfg(target_os = "linux")]
fn a_foreign_allocation_holding(bytes: &[u8]) -> std::os::fd::OwnedFd {
    use std::os::fd::FromRawFd as _;
    // SAFETY: a fresh memfd this test owns.
    let fd = unsafe { libc::memfd_create(c"foreign_dma_buf_stand_in".as_ptr(), 0) };
    assert!(fd >= 0, "memfd_create");
    let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    assert_eq!(written as usize, bytes.len(), "fill the stand-in");
    owned
}

#[cfg(target_os = "linux")]
#[test]
fn a_foreign_dma_buf_read_through_a_host_mapping_converts_through_its_padded_layout() {
    let Some(gpu) = gpu_or_skip(
        "a_foreign_dma_buf_read_through_a_host_mapping_converts_through_its_padded_layout",
    ) else {
        return;
    };
    let service = LiveSurfaceShareServiceForATest::wired_into(&gpu);
    let luma_row_pitch = 2 * PIXEL_WIDTH as usize;
    let chroma_offset = luma_row_pitch * PIXEL_HEIGHT as usize + 64;
    let allocation_byte_size = chroma_offset + luma_row_pitch * (PIXEL_HEIGHT as usize / 2) + 100;
    let planes = PixelPlanesWithinOneAllocation::new(
        PixelFormat::Nv12VideoRange,
        PIXEL_WIDTH,
        PIXEL_HEIGHT,
        allocation_byte_size as u64,
        vec![0, chroma_offset as u64],
        vec![luma_row_pitch as u64; 2],
        true,
    )
    .expect("the padded layout fits the allocation");
    let source_id = gpu
        .surface_store()
        .expect("the service's store is installed")
        .check_in_a_foreign_dma_buf_fd_with_a_pixel_plane_layout(
            a_foreign_allocation_holding(&nv12_bytes(
                allocation_byte_size,
                luma_row_pitch,
                chroma_offset,
            )),
            allocation_byte_size as u64,
            PixelFormat::Nv12VideoRange,
            PIXEL_WIDTH,
            PIXEL_HEIGHT,
            &planes,
        )
        .expect("the service adopts the foreign fd with its layout");
    let destination_id = a_storage_texture(&gpu, PIXEL_WIDTH, storage_and_copy_usage());
    let registry = EscalateHandleRegistry::new();
    registry
        .surface_color_conversion_session()
        .resolve_a_foreign_dma_buf_through_a_host_mapping(&gpu, &source_id)
        .expect("the foreign allocation maps and imports for GPU reads");

    for conversion in ["first", "second"] {
        convert_through_the_escalate_op(
            &gpu,
            &registry,
            &source_id,
            &destination_id,
            bt709_limited(),
        )
        .unwrap_or_else(|refusal| panic!("the {conversion} conversion is refused: {refusal}"));
        assert_white_left_and_black_right(&read_registered_texture(&gpu, &destination_id));
    }
    service.stop();
}
