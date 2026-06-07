// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Grayscale compute processor (Linux, engine-free).
//!
//! Resolves the incoming `VideoFrame` surface to a GPU [`Texture`], runs a
//! BT.601-luma grayscale SPIR-V **compute** kernel that writes into the next
//! slot of a pre-allocated output [`TextureRing`], and forwards the slot's
//! `surface_id` downstream. The in-process `Display` consumer resolves the
//! slot via Path 1 (`texture_cache`) — [`create_texture_ring`] registers
//! every slot at construction, so no surface-share registration is needed.
//!
//! Everything here goes through the engine-free `streamlib-plugin-sdk`:
//! kernel + ring are built on `GpuContextFullAccess` at setup (privileged),
//! and `process()` resolves + dispatches on `GpuContextLimitedAccess` (the
//! hot path never escalates). No raw `HostVulkanDevice`, so the cdylib stays
//! sound as a separately-built `.slpkg`.

use std::sync::atomic::{AtomicU64, Ordering};

use streamlib_plugin_sdk::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{
    ComputeBindingSpec, ComputeKernelDescriptor, TextureFormat, TextureRing, TextureUsages,
    VulkanComputeKernel, VulkanLayout,
};

use crate::_generated_::VideoFrame;

/// Output texture-ring depth — the engine's `MAX_FRAMES_IN_FLIGHT = 2` per
/// `docs/learnings/vulkan-frames-in-flight.md`. The prior slot may still be
/// sampled by `Display` while the kernel writes the next one.
const OUTPUT_RING_DEPTH: usize = 2;

/// Compute workgroup tile size; matches `local_size_x/y` in `grayscale.comp`.
const WORKGROUP_SIZE: u32 = 8;

/// Compiled grayscale compute SPIR-V (emitted by `build.rs` via `glslc`).
const GRAYSCALE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/grayscale.comp.spv"));

/// Binding layout for `grayscale.comp` (descriptor set 0):
///   0 = sampled input texture, 1 = storage output image.
const BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::sampled_texture(0),
    ComputeBindingSpec::storage_image(1),
];

struct ComputeBackend {
    kernel: VulkanComputeKernel,
    output_ring: TextureRing,
    width: u32,
    height: u32,
}

#[streamlib_plugin_sdk::sdk::processor("GrayscaleCompute")]
pub struct GrayscaleComputeProcessor {
    gpu_context: Option<GpuContextLimitedAccess>,
    backend: Option<ComputeBackend>,
    frame_count: AtomicU64,
}

impl streamlib_plugin_sdk::sdk::processors::ReactiveProcessor
    for GrayscaleComputeProcessor::Processor
{
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!("[GrayscaleCompute] setup (engine-free SPIR-V compute kernel)");
        self.gpu_context = Some(ctx.gpu_limited_access().clone());

        // setup() runs inside the engine's privileged lifecycle dispatch, so
        // `ctx.gpu_full_access()` is already privileged — building the kernel
        // + ring here (not via `gpu_limited_access().escalate(...)`) avoids
        // re-entering the escalate gate on the same thread.
        let full = ctx.gpu_full_access();

        // Camera in this example emits 1920×1080. The compute shader bounds-
        // checks against the output image size, and the kernel binds the
        // resolved input 1:1, so a mismatched source still produces a clean
        // (cropped) frame rather than reading out of bounds.
        let width = 1920u32;
        let height = 1080u32;

        let kernel = full.create_compute_kernel(&ComputeKernelDescriptor {
            label: "grayscale_compute",
            spv: GRAYSCALE_SPV,
            bindings: BINDINGS,
            push_constant_size: 0,
        })?;

        // Rgba8Unorm is universally storage-image-capable; grayscale writes
        // R=G=B so channel order is irrelevant to the result. STORAGE_BINDING
        // for the compute write, TEXTURE_BINDING for Display to sample,
        // COPY_SRC for the PNG sampler.
        let output_ring = full.create_texture_ring(
            width,
            height,
            TextureFormat::Rgba8Unorm,
            TextureUsages::STORAGE_BINDING
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_SRC,
            OUTPUT_RING_DEPTH,
        )?;

        tracing::info!(
            "[GrayscaleCompute] pre-allocated {OUTPUT_RING_DEPTH} output ring slots \
             ({width}x{height} RGBA8)"
        );

        self.backend = Some(ComputeBackend {
            kernel,
            output_ring,
            width,
            height,
        });
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            "[GrayscaleCompute] shutdown ({} frames)",
            self.frame_count.load(Ordering::Relaxed)
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: VideoFrame = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| Error::Configuration("GPU context not initialized".into()))?
            .clone();

        let backend = self.backend.as_ref().ok_or_else(|| {
            Error::Configuration("GrayscaleCompute: backend not initialized".into())
        })?;

        // Resolve the incoming camera surface to its GPU texture. The camera
        // dual-registers a texture-backed surface_id in texture_cache
        // (Path 1) leaving it SHADER_READ_ONLY_OPTIMAL — exactly what the
        // compute kernel samples.
        let input_registration = gpu_ctx.resolve_texture_registration_by_surface_id(
            &frame.surface_id,
            frame.texture_layout,
            frame.width,
            frame.height,
        )?;
        let input_texture = input_registration.texture().clone();

        // Rotate to the next output slot.
        let slot = backend.output_ring.acquire_next();
        let slot_surface_id = slot.surface_id().to_string();

        // Bind input (sampled) + output (storage image) and dispatch one
        // workgroup per 8×8 tile. `dispatch` submits + waits, so the slot is
        // populated on return.
        backend.kernel.set_sampled_texture(0, &input_texture)?;
        backend.kernel.set_storage_image(1, &slot.texture)?;
        let groups_x = backend.width.div_ceil(WORKGROUP_SIZE);
        let groups_y = backend.height.div_ceil(WORKGROUP_SIZE);
        backend.kernel.dispatch(groups_x, groups_y, 1)?;

        // The compute kernel leaves the storage-image output in GENERAL.
        // Update the slot registration so the downstream Display barriers
        // from a current_layout that matches reality.
        let slot_registration = gpu_ctx.resolve_texture_registration_by_surface_id(
            &slot_surface_id,
            None,
            slot.texture.width(),
            slot.texture.height(),
        )?;
        slot_registration.update_layout(VulkanLayout::GENERAL);

        let output_frame = VideoFrame {
            surface_id: slot_surface_id,
            width: slot.texture.width(),
            height: slot.texture.height(),
            timestamp_ns: frame.timestamp_ns.clone(),
            fps: frame.fps,
            texture_layout: Some(VulkanLayout::GENERAL.0),
            color_info: frame.color_info.clone(),
            mastering_display: frame.mastering_display.clone(),
            content_light: frame.content_light.clone(),
        };
        self.outputs.write("video_out", &output_frame)?;
        self.frame_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}
