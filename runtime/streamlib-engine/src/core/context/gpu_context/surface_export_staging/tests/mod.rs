// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The surface export staging's tests, split by concern, and the fixtures they share.

use super::*;

mod cpu_readback_residency_and_display_importable_tests;
mod device_export_and_write_back_tests;

use crate::core::rhi::{PixelBuffer, TextureDescriptor, TextureUsages};
use crate::host_rhi::{VulkanAccess, VulkanStage};
use streamlib_surface_client::ViewedFrameReadBy;

const SURFACE_WIDTH: u32 = 64;
const SURFACE_HEIGHT: u32 = 64;
/// The pixels the bag was published with.
const PUBLISHED_FRAME_STAMP: u8 = 0x11;
/// What the producer's ring slot holds once it has come around again.
const TWO_FRAMES_LATER_STAMP: u8 = 0x77;
/// What a consumer writes through a writable export.
const CONSUMER_EDIT_STAMP: u8 = 0x5a;
/// A kernel output's pixels — texture-backed, no pooled member.
const KERNEL_OUTPUT_STAMP: u8 = 0x2b;

fn gpu_context_or_skip() -> Option<GpuContext> {
    match GpuContext::init_for_platform() {
        Ok(gpu) => Some(gpu),
        Err(_) => {
            println!("Skipping - no GPU device available");
            None
        }
    }
}

fn pooled_backing_host_mapping(buffer: &PixelBuffer) -> (*mut u8, usize) {
    let base_address = buffer.plane_base_address(0);
    assert!(
        !base_address.is_null(),
        "a pooled buffer must be host-mapped for this fixture to reach its pixels"
    );
    (base_address, buffer.plane_size(0) as usize)
}

fn stamp_every_byte_of_pooled_backing(buffer: &PixelBuffer, stamp: u8) {
    let (base_address, byte_count) = pooled_backing_host_mapping(buffer);
    unsafe { std::ptr::write_bytes(base_address, stamp, byte_count) };
}

fn read_pooled_backing_bytes(buffer: &PixelBuffer) -> Vec<u8> {
    let (base_address, byte_count) = pooled_backing_host_mapping(buffer);
    unsafe { std::slice::from_raw_parts(base_address, byte_count) }.to_vec()
}

fn assert_every_byte_is(bytes: &[u8], expected: u8, asserted_subject: &str) {
    assert!(!bytes.is_empty(), "{asserted_subject} came back empty");
    if let Some((index, found)) = bytes
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| *byte != expected)
    {
        panic!("{asserted_subject}: byte {index} is {found:#04x}, expected {expected:#04x}");
    }
}

/// A DEVICE_LOCAL texture a producer renders into and registers under
/// a surface id — the camera's ring slot and a kernel's output are
/// both this shape.
fn create_producer_owned_texture(gpu: &GpuContext) -> Texture {
    let descriptor =
        TextureDescriptor::new(SURFACE_WIDTH, SURFACE_HEIGHT, TextureFormat::Rgba8Unorm)
            .with_usage(
                TextureUsages::COPY_SRC
                    | TextureUsages::COPY_DST
                    | TextureUsages::TEXTURE_BINDING
                    | TextureUsages::STORAGE_BINDING,
            );
    gpu.device()
        .create_texture(&descriptor)
        .expect("producer-owned texture")
}

/// The staging is DEVICE_LOCAL OPAQUE_FD memory, so the host reaches
/// its bytes only through a copy into a mapped allocation — the same
/// hop a CUDA consumer's device-to-host copy makes.
fn read_device_export_staging_into_host_bytes(
    gpu: &GpuContext,
    staging: &SurfaceExportStaging,
) -> Vec<u8> {
    let byte_size = staging.staging_byte_size();
    let host_readback = gpu
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                .expect("a non-zero staging"),
        )
        .expect("host-visible readback allocation");
    let mut recorder = gpu
        .create_command_recorder("device_export_staging_test_readback")
        .expect("readback recorder");
    recorder.begin().expect("begin readback");
    recorder
        .record_copy_buffer_to_buffer(
            staging.staging_buffer().expect("a buffer staging").as_ref(),
            &host_readback,
            byte_size,
        )
        .expect("copy the staging into the readback allocation");
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

/// The device-side write a consumer would make through a writable
/// export, before asking for it to be published back.
fn stamp_every_byte_of_device_export_staging(
    gpu: &GpuContext,
    staging: &SurfaceExportStaging,
    stamp: u8,
) {
    let byte_size = staging.staging_byte_size();
    let host_upload = gpu
        .acquire_storage_buffer(
            &crate::core::rhi::TensorStorageBufferLayout::of_bytes(byte_size)
                .expect("a non-zero staging"),
        )
        .expect("host-visible upload allocation");
    let mapped = host_upload.mapped_ptr();
    assert!(!mapped.is_null(), "the upload allocation must be mapped");
    unsafe { std::ptr::write_bytes(mapped, stamp, byte_size as usize) };

    let mut recorder = gpu
        .create_command_recorder("device_export_staging_test_upload")
        .expect("upload recorder");
    recorder.begin().expect("begin upload");
    recorder
        .record_copy_buffer_to_buffer(
            &host_upload,
            staging.staging_buffer().expect("a buffer staging").as_ref(),
            byte_size,
        )
        .expect("copy the edit into the staging");
    recorder.submit_and_wait().expect("submit the edit");
}

struct RotatingProducerPublishedFrame {
    surface_id: String,
    /// Held for the fixture's life: the pool reclaims a slot when the
    /// last handle drops, and this frame must stay the bag's frame.
    _pooled_backing: PixelBuffer,
    producer_ring_texture: Texture,
    ring_upload_source: PixelBuffer,
}

impl RotatingProducerPublishedFrame {
    fn publish(gpu: &GpuContext, stamp: u8) -> Self {
        let (pool_id, pooled_backing) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the frame's pooled backing");
        let surface_id = pool_id.to_string();
        stamp_every_byte_of_pooled_backing(&pooled_backing, stamp);

        let producer_ring_texture = create_producer_owned_texture(gpu);
        let (_, ring_upload_source) = gpu
            .acquire_pixel_buffer(SURFACE_WIDTH, SURFACE_HEIGHT, PixelFormat::Rgba32)
            .expect("acquire the ring's upload source");
        gpu.register_texture_with_layout(
            &surface_id,
            producer_ring_texture.clone(),
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
        );

        let published = Self {
            surface_id,
            _pooled_backing: pooled_backing,
            producer_ring_texture,
            ring_upload_source,
        };
        published.advance_ring_texture_to(gpu, stamp);
        published
    }

    /// The producer comes around to this ring slot again: the slot is
    /// rewritten in place, the pooled backing the bag named is not.
    fn advance_ring_texture_to(&self, gpu: &GpuContext, stamp: u8) {
        stamp_every_byte_of_pooled_backing(&self.ring_upload_source, stamp);
        gpu.copy_pixel_buffer_to_texture(
            &self.ring_upload_source,
            &self.producer_ring_texture,
            &self.surface_id,
            SURFACE_WIDTH,
            SURFACE_HEIGHT,
        )
        .expect("overwrite the producer's ring slot");
    }
}
