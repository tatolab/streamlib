// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib_adapter_cpu_readback::tests::conformance` — runs the
//! public `run_conformance` suite from `streamlib-adapter-abi` against
//! a real cpu-readback adapter wired to a host-allocated DMA-BUF
//! `VkImage`, per-plane staging buffers, and an exportable timeline
//! semaphore.
//!
//! Same eight contracts the Vulkan / OpenGL adapters pass: acquire/drop
//! pairs, parallel reads, `WriteContended` on contention,
//! `try_acquire_*` returning `Ok(None)` on contention, and Send+Sync
//! under multi-thread reads.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use streamlib_adapter_abi::testing::{empty_surface, run_conformance};
use streamlib_adapter_abi::{AdapterError, StreamlibSurface, SurfaceAdapter, SurfaceId};

use common::HostFixture;

struct ConformanceFactory<'a> {
    fixture: &'a HostFixture,
}

impl<'a> streamlib_adapter_abi::testing::ConformanceSurfaceFactory
    for ConformanceFactory<'a>
{
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        self.fixture.register_surface(id, 64, 64)
    }
}

#[test]
fn cpu_readback_adapter_passes_run_conformance() {
    let fixture = match HostFixture::try_new() {
        Some(f) => f,
        None => {
            println!(
                "cpu-readback conformance: skipping — no Vulkan device available"
            );
            return;
        }
    };

    let factory = ConformanceFactory { fixture: &fixture };
    run_conformance(&*fixture.adapter, factory);

    let bogus = empty_surface(0xdead_beef);
    match fixture.adapter.acquire_read(&bogus) {
        Err(AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 0xdead_beef);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}
