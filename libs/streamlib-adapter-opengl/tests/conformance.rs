// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_opengl::tests::conformance` — runs the public
//! `run_conformance` suite from `streamlib-adapter-abi` against a
//! real `OpenGlSurfaceAdapter` wired to a host-allocated DMA-BUF
//! render-target image and an EGL surfaceless context.
//!
//! Exercises the same eight contracts `MockAdapter` passes
//! (acquire/drop pairs, parallel reads, `WriteContended` on
//! contention, `try_acquire_*` returning `Ok(None)`, multi-thread
//! Send+Sync). A green run confirms the trait shape is honored — it
//! does NOT prove rendering correctness; that's the
//! `fbo_completeness` / `round_trip_render_to_surface` /
//! `sample_from_surface` tests.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceId};

use common::HostFixture;

struct ConformanceFactory<'a> {
    fixture: &'a HostFixture,
}

impl streamlib_adapter_abi::testing::ConformanceSurfaceFactory for ConformanceFactory<'_> {
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        // 64×64 BGRA8 — small enough to keep the per-surface
        // allocation cheap, large enough that the modifier-aware
        // import path is exercised.
        self.fixture.register_surface(id, 64, 64).descriptor
    }
}

#[test]
fn opengl_adapter_passes_run_conformance() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!("opengl-adapter conformance: skipping — no Vulkan or no EGL");
            return;
        }
    };
    let factory = ConformanceFactory { fixture: &fixture };
    run_conformance(&*fixture.adapter, factory);

    // Bonus: an unknown surface id must surface as SurfaceNotFound,
    // not as a generic "WriteContended unknown".
    let bogus = empty_surface(0xdead_beef);
    match fixture.adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound for unknown id, got {other:?}"),
    }
}
