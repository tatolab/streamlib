// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-rustc / cross-dep-graph dlopen fixture for issue #927.
//!
//! This crate is the empirical gate for the structural cross-repo
//! plugin distribution claim from CLAUDE.md → "Plugin Distribution
//! Model — the cross-repo dream" and PR #918's β-shape Phase D work.
//!
//! What it locks: a cdylib built in a **standalone Cargo workspace**
//! (this crate's `Cargo.toml` carries its own `[workspace]` table, so
//! `cargo build` resolves a separate `Cargo.lock` with **deliberately
//! divergent transitive dep versions** vs the host streamlib
//! workspace) loads cleanly into the host runtime through
//! `runtime.load_project(...)` and round-trips every #917 β-shape
//! return type through create + clone + drop without panic.
//!
//! Per the issue body's "Approach" section, this fixture rides
//! Option 1 (same-rustc, mismatched dep graph). Cross-rustc-version
//! independence is **structural by construction**: every type that
//! crosses the cdylib boundary in #917 is `#[repr(C)]` with a
//! byte-pinned layout regression test in `streamlib-plugin-abi`.
//! When β-shape coverage is complete (Phase E #907 + Phase F #908),
//! a follow-up CI matrix can build this same fixture under a
//! different rustc minor without source changes to upgrade Option 1
//! → Option 2.
//!
//! ## β-shape types exercised
//!
//! From PR #918's list of seven refactored return types, this
//! fixture exercises the four that cover the two distinct shapes:
//!
//! - **`Arc`-handle + Clone-yes** (5 of 7 types in PR #918):
//!   - `TextureRing` — engine helper, no shader inputs.
//!   - `RhiColorConverter` — built from `(src, dst)` `PixelFormat`s.
//!   - `VulkanComputeKernel` — built from a SPIR-V blob shipped in
//!     this crate's `OUT_DIR`.
//!
//! - **`Box`-handle + NOT-Clone** (1 of 7, locked by
//!   `compile_fail` doctest in plugin-abi):
//!   - `RhiCommandRecorder` — create + drop, no clone.
//!
//! The remaining three Arc-based types from PR #918 —
//! `VulkanGraphicsKernel`, `VulkanRayTracingKernel`,
//! `VulkanAccelerationStructure` — share the **same Arc-handle +
//! Clone-yes β-shape pattern** as `VulkanComputeKernel`, dispatched
//! through their own dedicated clone/drop vtable slots whose byte
//! offsets are independently pinned in
//! `streamlib-plugin-abi`'s `GpuContextFullAccessVTable` layout
//! regression test. The compute kernel here exercises the pattern
//! end-to-end; the layout regression test locks each slot's offset
//! per-type; the host-side callback for each slot is exercised by
//! the engine's own unit tests (`vulkan_compute_kernel::tests`,
//! `vulkan_graphics_kernel::tests`,
//! `vulkan_ray_tracing_kernel::tests`,
//! `vulkan_acceleration_structure::tests`). Extending this fixture
//! to exercise the three additional types directly is a follow-up.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::ManualProcessor;
use streamlib::sdk::rhi::{
    ComputeBindingSpec, ComputeKernelDescriptor, PixelFormat, TextureFormat, TextureUsages,
};

/// SPIR-V for the trivial compute kernel compiled by `build.rs`.
#[cfg(target_os = "linux")]
const TRIVIAL_COMPUTE_SPV: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/trivial_compute.spv"));

#[cfg(target_os = "linux")]
const TRIVIAL_COMPUTE_BINDINGS: &[ComputeBindingSpec] =
    &[ComputeBindingSpec::storage_buffer(0)];

#[streamlib::sdk::processor("BetaShapeRoundTripProcessor")]
pub struct BetaShapeRoundTrip {}

impl ManualProcessor for BetaShapeRoundTrip::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let output_path = self.config.output_path.clone();

        // Exercise β-shape Create + Clone + Drop inside an escalate
        // scope. The vtable-dispatched FullAccess wraps every
        // `create_*` call through the `extern "C"` slot pair on
        // `GpuContextFullAccessVTable`, and every Clone / Drop on the
        // returned β-shape routes through the corresponding
        // `clone_<type>` / `drop_<type>` vtable slot. A regression
        // in any of those slots — wrong offset, wrong Arc/Box
        // arithmetic, layout drift in the β-shape struct itself —
        // surfaces as a panic at the FFI boundary (caught by
        // `run_host_extern_c` and reported as an error from the
        // closure) or as the file containing an `ERR:` line.
        let result: Result<String> = ctx.gpu_limited_access().escalate(|full| {
            let mut summary = String::new();

            // -------- TextureRing (Arc-handle + Clone) --------
            #[cfg(target_os = "linux")]
            {
                let ring = full.create_texture_ring(
                    64,
                    64,
                    TextureFormat::Rgba8Unorm,
                    TextureUsages::TEXTURE_BINDING | TextureUsages::STORAGE_BINDING,
                    2,
                )?;
                // Cross the `clone_texture_ring` vtable slot.
                let ring_clone = ring.clone();
                // Cross `drop_texture_ring` twice (clone + original).
                drop(ring_clone);
                drop(ring);
                summary.push_str("TextureRing:OK\n");
            }
            #[cfg(not(target_os = "linux"))]
            {
                summary.push_str("TextureRing:SKIPPED_NON_LINUX\n");
            }

            // -------- RhiColorConverter (Arc-handle + Clone) --------
            #[cfg(target_os = "linux")]
            {
                let cc = full.color_converter(PixelFormat::Bgra32, PixelFormat::Rgba32)?;
                let cc_clone = cc.clone();
                drop(cc_clone);
                drop(cc);
                summary.push_str("RhiColorConverter:OK\n");
            }
            #[cfg(not(target_os = "linux"))]
            {
                summary.push_str("RhiColorConverter:SKIPPED_NON_LINUX\n");
            }

            // -------- RhiCommandRecorder (Box-handle + NOT Clone) --------
            #[cfg(target_os = "linux")]
            {
                let recorder = full.create_command_recorder("cross-rustc-fixture")?;
                // No Clone — compile_fail doctest in plugin-abi
                // locks the NOT-Clone invariant. Drop crosses the
                // `drop_command_recorder` vtable slot.
                drop(recorder);
                summary.push_str("RhiCommandRecorder:OK\n");
            }
            #[cfg(not(target_os = "linux"))]
            {
                summary.push_str("RhiCommandRecorder:SKIPPED_NON_LINUX\n");
            }

            // -------- VulkanComputeKernel (Arc-handle + Clone) --------
            #[cfg(target_os = "linux")]
            {
                let kernel = full.create_compute_kernel(&ComputeKernelDescriptor {
                    label: "cross-rustc-fixture-trivial",
                    spv: TRIVIAL_COMPUTE_SPV,
                    bindings: TRIVIAL_COMPUTE_BINDINGS,
                    push_constant_size: 0,
                })?;
                let kernel_clone = kernel.clone();
                drop(kernel_clone);
                drop(kernel);
                summary.push_str("VulkanComputeKernel:OK\n");
            }
            #[cfg(not(target_os = "linux"))]
            {
                summary.push_str("VulkanComputeKernel:SKIPPED_NON_LINUX\n");
            }

            Ok(summary)
        });

        let line = match result {
            Ok(summary) => format!("OK\n{summary}"),
            Err(e) => format!("ERR:{e}"),
        };
        std::fs::write(&output_path, &line).map_err(|e| {
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
