// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-rustc / cross-dep-graph dlopen fixture for issue #927.
//!
//! Companion to PR #918's β-shape Phase D work. The fixture builds in
//! a standalone Cargo workspace (own `[workspace]` table → own
//! `Cargo.lock`) with `=`-pinned older `serde` / `tracing` transitive
//! deps, so cargo resolves the cdylib against a deliberately
//! divergent crate graph from the host streamlib workspace.
//!
//! What the integration test surfaces:
//!
//! - **Build-level**: `cargo build` against this fixture's Cargo.toml
//!   produces a `.so` against streamlib-sdk compiled with different
//!   transitive deps than the host's compiled artifacts — proves the
//!   plugin author can ship a `.so` without coordinating dep graphs
//!   with the host.
//! - **Load-level**: `Runner::load_project(...)` dlopens that `.so`
//!   and the host's `STREAMLIB_PLUGIN` ABI accepts the cdylib's
//!   exported symbol shape — proves the FFI surface from #918 is
//!   layout-stable across the divergent compiles.
//! - **Dispatch-level**: each #918 β-shape return type
//!   (`VulkanComputeKernel`, `VulkanGraphicsKernel`,
//!   `VulkanRayTracingKernel`, `TextureRing`, `RhiColorConverter`,
//!   `VulkanAccelerationStructure`, `RhiCommandRecorder`) is
//!   constructed via the FullAccess vtable inside an escalate scope,
//!   cloned (or — for the Box-handle `RhiCommandRecorder` β-shape —
//!   only dropped, since the type is `!Clone`), and dropped from
//!   cdylib code. Every Create / Clone / Drop transits through the
//!   per-type host-installed `clone_<type>` / `drop_<type>` vtable
//!   slot. A FFI-boundary panic surfaces as `ERR:` in the result
//!   file; correct dispatch surfaces as `OK` + a per-type status
//!   line.
//!
//! What this test does NOT prove on its own — these are locked
//! elsewhere:
//!
//! - Per-`extern "C"` slot byte offset → `streamlib-plugin-abi`'s
//!   `offset_of!` layout regression tests.
//! - Host-side callback bodies for each clone/drop slot → the
//!   engine's own per-type unit tests
//!   (`vulkan_compute_kernel::tests` etc.).
//! - True cross-rustc-version (different rustc minor) → structural
//!   by `#[repr(C)]` design; upgrading Option 1 → Option 2 (rustc
//!   matrix in CI) requires no source changes here.
//!
//! Ray-tracing coverage is conditional on the test host advertising
//! `supports_ray_tracing_pipeline()`. On hosts without RT the test
//! records `VulkanRayTracingKernel:SKIPPED_NO_RT_SUPPORT` plus the
//! matching skip for `VulkanAccelerationStructure` (the BLAS build
//! path shares the RT feature gate); the integration test treats the
//! skip lines as a soft-pass rather than a missing-coverage failure.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{
    AttachmentFormats, ColorBlendState, ColorWriteMask, ComputeBindingSpec,
    ComputeKernelDescriptor, DepthStencilState, GraphicsBindingSpec, GraphicsDynamicState,
    GraphicsKernelDescriptor, GraphicsPipelineState, GraphicsPushConstants,
    GraphicsShaderStageFlags, GraphicsStage, MultisampleState, PixelFormat, PrimitiveTopology,
    RasterizationState, RayTracingBindingSpec, RayTracingKernelDescriptor,
    RayTracingPushConstants, RayTracingShaderGroup, RayTracingShaderStageFlags, RayTracingStage,
    TextureFormat, TextureUsages, VertexInputState,
};

#[cfg(target_os = "linux")]
const TRIVIAL_COMPUTE_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/trivial_compute.spv"));
#[cfg(target_os = "linux")]
const TRIVIAL_VERT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trivial_vert.spv"));
#[cfg(target_os = "linux")]
const TRIVIAL_FRAG_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trivial_frag.spv"));
#[cfg(target_os = "linux")]
const TRIVIAL_RGEN_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trivial_rgen.spv"));
#[cfg(target_os = "linux")]
const TRIVIAL_RMISS_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trivial_rmiss.spv"));
#[cfg(target_os = "linux")]
const TRIVIAL_RCHIT_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/trivial_rchit.spv"));

#[cfg(target_os = "linux")]
const TRIVIAL_COMPUTE_BINDINGS: &[ComputeBindingSpec] =
    &[ComputeBindingSpec::storage_buffer(0)];

#[streamlib::sdk::processor("BetaShapeRoundTripProcessor")]
pub struct BetaShapeRoundTrip {}

/// Run a Create+Clone+Drop sweep over every #918 β-shape return type
/// inside an escalate scope so FullAccess methods route through the
/// FFI vtable (not the in-process `Boxed` handle). Called once from
/// `start()` — setup() leaves the sweep alone because the FullAccess
/// vtable instance is the same across both lifecycles, and BLAS +
/// RT-kernel construction make doubling the sweep expensive without
/// adding distinct vtable-surface coverage.
#[cfg(target_os = "linux")]
fn run_beta_shape_round_trip(ctx: &RuntimeContextFullAccess<'_>) -> Result<String> {
    let gpu_limited = ctx.gpu_limited_access();
    let ring = gpu_limited.escalate(|full| {
        full.create_texture_ring(
            64,
            64,
            TextureFormat::Rgba8Unorm,
            TextureUsages::COPY_DST
                | TextureUsages::TEXTURE_BINDING
                | TextureUsages::STORAGE_BINDING,
            2,
        )
    })?;

    // -------- TextureRing slot β-shape end-to-end (#947) --------
    //
    // The slot β-shape's per-method dispatch (acquire_next /
    // copy_pixel_buffer_to_slot / slot) routes through the per-type
    // TextureRingMethodsVTable in cdylib mode. This block exercises
    // every slot from cdylib code, asserts cached POD fields round-
    // trip, then runs the per-frame copy_pixel_buffer_to_slot
    // primitive against a Limited-safe pixel buffer — proving every
    // v2 vtable slot survives the divergent-dep-graph build.
    {
        if ring.len() != 2 {
            return Err(Error::Runtime(format!(
                "TextureRing: expected len=2, got {}",
                ring.len()
            )));
        }
        let slot0 = ring.acquire_next();
        let slot1 = ring.acquire_next();
        if slot0.surface_id() == slot1.surface_id() {
            return Err(Error::Runtime(
                "TextureRing: acquire_next returned the same slot twice in a row".into(),
            ));
        }
        if slot0.texture.width() != 64 || slot0.texture.height() != 64 {
            return Err(Error::Runtime(format!(
                "TextureRing: slot texture dims ({}, {}) don't match ring construction (64, 64)",
                slot0.texture.width(),
                slot0.texture.height()
            )));
        }
        let direct_slot0 = ring
            .slot(0)
            .ok_or_else(|| Error::Runtime("TextureRing: ring.slot(0) returned None".into()))?;
        if direct_slot0.slot_index() != 0 {
            return Err(Error::Runtime(format!(
                "TextureRing: ring.slot(0) returned slot with slot_index = {}",
                direct_slot0.slot_index()
            )));
        }
        // copy_pixel_buffer_to_slot: stage a pixel buffer, then write
        // it into the slot's pre-allocated texture via the v2
        // copy_pixel_buffer_to_slot vtable slot.
        let (_pool_id, pixel_buffer) = gpu_limited
            .acquire_pixel_buffer(64, 64, PixelFormat::Rgba32)
            .map_err(|e| Error::Runtime(format!("acquire_pixel_buffer: {e}")))?;
        ring.copy_pixel_buffer_to_slot(&slot0, &pixel_buffer, 64, 64)
            .map_err(|e| Error::Runtime(format!("copy_pixel_buffer_to_slot: {e}")))?;
    }

    let ring_clone = ring.clone();
    drop(ring_clone);
    drop(ring);
    gpu_limited.escalate(|full| {
        let mut summary = String::new();
        summary.push_str("TextureRing:OK\n");

        // -------- RhiColorConverter (Arc-handle + Clone) --------
        let cc = full.color_converter(PixelFormat::Bgra32, PixelFormat::Rgba32)?;
        let cc_clone = cc.clone();
        drop(cc_clone);
        drop(cc);
        summary.push_str("RhiColorConverter:OK\n");

        // -------- RhiCommandRecorder (Box-handle + NOT Clone) --------
        let recorder = full.create_command_recorder("cross-rustc-fixture")?;
        drop(recorder);
        summary.push_str("RhiCommandRecorder:OK\n");

        // -------- VulkanComputeKernel (Arc-handle + Clone) --------
        let kernel = full.create_compute_kernel(&ComputeKernelDescriptor {
            label: "cross-rustc-fixture-compute",
            spv: TRIVIAL_COMPUTE_SPV,
            bindings: TRIVIAL_COMPUTE_BINDINGS,
            push_constant_size: 0,
        })?;
        let kernel_clone = kernel.clone();
        drop(kernel_clone);
        drop(kernel);
        summary.push_str("VulkanComputeKernel:OK\n");

        // -------- VulkanGraphicsKernel (Arc-handle + Clone) --------
        let stages = [
            GraphicsStage::vertex(TRIVIAL_VERT_SPV),
            GraphicsStage::fragment(TRIVIAL_FRAG_SPV),
        ];
        let bindings: &[GraphicsBindingSpec] = &[];
        let graphics_kernel = full.create_graphics_kernel(&GraphicsKernelDescriptor {
            label: "cross-rustc-fixture-graphics",
            stages: &stages,
            bindings,
            push_constants: GraphicsPushConstants {
                size: 0,
                stages: GraphicsShaderStageFlags::NONE,
            },
            pipeline_state: GraphicsPipelineState {
                topology: PrimitiveTopology::TriangleList,
                vertex_input: VertexInputState::None,
                rasterization: RasterizationState::default(),
                multisample: MultisampleState::default(),
                depth_stencil: DepthStencilState::Disabled,
                color_blend: ColorBlendState::Disabled {
                    color_write_mask: ColorWriteMask::RGBA,
                },
                attachment_formats: AttachmentFormats::color_only(TextureFormat::Bgra8Unorm),
                dynamic_state: GraphicsDynamicState::ViewportScissor,
            },
            descriptor_sets_in_flight: 2,
        })?;
        let graphics_kernel_clone = graphics_kernel.clone();
        drop(graphics_kernel_clone);
        drop(graphics_kernel);
        summary.push_str("VulkanGraphicsKernel:OK\n");

        // -------- VulkanAccelerationStructure + VulkanRayTracingKernel --------
        // Both ride the same `VK_KHR_ray_tracing_pipeline` /
        // `VK_KHR_acceleration_structure` feature gate. On hosts that
        // lack RT (or where the engine's RT probe declined to enable
        // it), record SKIP without failing — the structural β-shape
        // argument from #918 is identical for these types as for
        // VulkanComputeKernel which IS exercised on every host.
        if full.supports_ray_tracing_pipeline() {
            // VulkanAccelerationStructure: trivial single-triangle BLAS.
            // Exercises the #955 v8 build_triangles_blas out-params:
            // the cdylib-minted β-shape must carry real device_address,
            // storage_size, and kind (no placeholder zeros), and its
            // label() method must round-trip through the new
            // VulkanAccelerationStructureMethodsVTable::label slot.
            let vertices = [
                0.0f32, 0.0, 0.0, //
                1.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, //
            ];
            let indices = [0u32, 1, 2];
            let blas_label = "cross-rustc-fixture-blas";
            let blas = full.build_triangles_blas(
                blas_label,
                &vertices,
                &indices,
            )?;
            // Real device address from the v8 out-param (not the
            // placeholder zero the pre-v8 cdylib path produced).
            if blas.device_address() == 0 {
                return Err(Error::Runtime(
                    "VulkanAccelerationStructure: build_triangles_blas                      produced cached_device_address=0 (v8 out-param                      not surfaced or BLAS truly has no device address)"
                        .into(),
                ));
            }
            if blas.storage_size() == 0 {
                return Err(Error::Runtime(
                    "VulkanAccelerationStructure: build_triangles_blas                      produced cached_storage_size=0 (v8 out-param not                      surfaced or BLAS build skipped storage allocation)"
                        .into(),
                ));
            }
            // kind() reads cached_kind; build_triangles_blas always
            // mints BottomLevel (the host's from_arc_into_raw writes
            // out_kind = 0 for BLAS, 1 for TLAS).
            if blas.kind()
                != streamlib::sdk::engine::host_rhi::AccelerationStructureKind::BottomLevel
            {
                return Err(Error::Runtime(format!(
                    "VulkanAccelerationStructure: build_triangles_blas                      produced kind = {:?}, expected BottomLevel",
                    blas.kind()
                )));
            }
            // label() routes through the v2 methods vtable slot in
            // cdylib mode (host_inner panics if reached). Round-trip
            // must match what we passed at build time exactly.
            let round_tripped = blas.label();
            if round_tripped != blas_label {
                return Err(Error::Runtime(format!(
                    "VulkanAccelerationStructure: label round-trip                      mismatch — passed {blas_label:?} but got {:?}",
                    round_tripped
                )));
            }
            let blas_clone = blas.clone();
            drop(blas_clone);
            drop(blas);
            summary.push_str("VulkanAccelerationStructure:OK\n");

            // VulkanRayTracingKernel: minimal rgen/rmiss/rchit triple.
            let rt_stages = [
                RayTracingStage::ray_gen(TRIVIAL_RGEN_SPV),
                RayTracingStage::miss(TRIVIAL_RMISS_SPV),
                RayTracingStage::closest_hit(TRIVIAL_RCHIT_SPV),
            ];
            let rt_groups = [
                RayTracingShaderGroup::General { general: 0 },
                RayTracingShaderGroup::General { general: 1 },
                RayTracingShaderGroup::TrianglesHit {
                    closest_hit: Some(2),
                    any_hit: None,
                },
            ];
            let rt_bindings = [
                RayTracingBindingSpec::acceleration_structure(
                    0,
                    RayTracingShaderStageFlags::RAYGEN,
                ),
                RayTracingBindingSpec::storage_image(
                    1,
                    RayTracingShaderStageFlags::RAYGEN,
                ),
            ];
            let rt_kernel = full.create_ray_tracing_kernel(&RayTracingKernelDescriptor {
                label: "cross-rustc-fixture-rt",
                stages: &rt_stages,
                groups: &rt_groups,
                bindings: &rt_bindings,
                push_constants: RayTracingPushConstants::NONE,
                max_recursion_depth: 1,
            })?;
            let rt_kernel_clone = rt_kernel.clone();
            drop(rt_kernel_clone);
            drop(rt_kernel);
            summary.push_str("VulkanRayTracingKernel:OK\n");
        } else {
            summary.push_str("VulkanAccelerationStructure:SKIPPED_NO_RT_SUPPORT\n");
            summary.push_str("VulkanRayTracingKernel:SKIPPED_NO_RT_SUPPORT\n");
        }

        Ok(summary)
    })
}

impl ManualProcessor for BetaShapeRoundTrip::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        // Setup intentionally empty. Running the full β-shape sweep
        // here too would double the GPU work (BLAS build + RT
        // kernel pipeline construction each take real time) and
        // duplicate coverage that doesn't differ between lifecycles —
        // the `RuntimeContextFullAccess` handed to setup() and start()
        // wrap the same `GpuContextFullAccess` β-shape with the same
        // host-side vtable instance. The single sweep in `start()` is
        // sufficient to exercise the FullAccess vtable surface and
        // the per-β-shape clone/drop slots.
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        let start_result: Result<String> = (|| {
            #[cfg(target_os = "linux")]
            {
                run_beta_shape_round_trip(ctx)
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = ctx;
                Ok("SKIPPED_NON_LINUX\n".to_string())
            }
        })();

        let body = match start_result {
            Ok(summary) => format!("OK\n{summary}"),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &body).map_err(|e| {
            Error::Runtime(format!(
                "BetaShapeRoundTripProcessor: write {output_path}: {e}"
            ))
        })?;
        Ok(())
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

streamlib_plugin_abi::export_plugin!(crate::BetaShapeRoundTrip::Processor);
