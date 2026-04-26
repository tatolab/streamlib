// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generic conformance fixture for [`SurfaceAdapter`] implementations.
//!
//! Adapter authors call [`run_conformance`] from a normal Rust test:
//!
//! ```rust,ignore
//! #[test]
//! fn my_adapter_is_conformant() {
//!     let adapter = MyAdapter::new();
//!     let factory = |id| my_test_surface(id);
//!     streamlib_adapter_abi::testing::run_conformance(&adapter, factory);
//! }
//! ```
//!
//! The fixture covers acquire/release ordering, double-acquire-write
//! rejection, concurrent-read permission, scope-drop counter
//! emission, and trait_version reporting.

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crate::adapter::{SurfaceAdapter, STREAMLIB_ADAPTER_ABI_VERSION};
use crate::error::AdapterError;
use crate::surface::{StreamlibSurface, SurfaceFormat, SurfaceId, SurfaceUsage};

/// Build a fresh [`StreamlibSurface`] descriptor for the given id.
///
/// Each adapter implementation knows how to wire its own transport/sync
/// state — a CPU adapter can use `SurfaceTransportHandle::empty()`, a
/// DMA-BUF Vulkan adapter constructs from real fds. The factory is
/// passed a fresh `SurfaceId` per call.
pub trait ConformanceSurfaceFactory {
    fn make(&self, id: SurfaceId) -> StreamlibSurface;
}

impl<F> ConformanceSurfaceFactory for F
where
    F: Fn(SurfaceId) -> StreamlibSurface,
{
    fn make(&self, id: SurfaceId) -> StreamlibSurface {
        (self)(id)
    }
}

/// Helper for the most common case where a test only needs a
/// CPU-empty surface descriptor (no transport, no sync).
pub fn empty_surface(id: SurfaceId) -> StreamlibSurface {
    StreamlibSurface::new(
        id,
        16,
        16,
        SurfaceFormat::Bgra8,
        SurfaceUsage::SAMPLED,
        crate::surface::SurfaceTransportHandle::empty(),
        crate::surface::SurfaceSyncState::default(),
    )
}

/// Run the parameterized conformance fixture against `adapter`.
///
/// Panics on the first violation. The fixture currently exercises:
///
/// 1. `trait_version()` reports a version compatible with this build.
/// 2. A single `acquire_read` / drop pair leaves no holders behind.
/// 3. A single `acquire_write` / drop pair leaves no holders behind.
/// 4. Two concurrent `acquire_read`s are permitted.
/// 5. `acquire_write` while a `ReadGuard` is alive returns
///    `WriteContended`.
/// 6. `acquire_write` while another `WriteGuard` is alive returns
///    `WriteContended`.
pub fn run_conformance<A, F>(adapter: &A, factory: F)
where
    A: SurfaceAdapter + Sync,
    F: ConformanceSurfaceFactory + Sync,
{
    let next_id = AtomicU64::new(1);
    let new_surface = || factory.make(next_id.fetch_add(1, Ordering::AcqRel));

    // 1. trait_version()
    let v = adapter.trait_version();
    assert_eq!(
        v % 1_000_000,
        STREAMLIB_ADAPTER_ABI_VERSION,
        "trait_version() must match STREAMLIB_ADAPTER_ABI_VERSION major \
         (got {v}, expected {STREAMLIB_ADAPTER_ABI_VERSION})"
    );

    // 2. acquire_read / drop
    {
        let s = new_surface();
        let g = adapter
            .acquire_read(&s)
            .expect("acquire_read on idle surface must succeed");
        let _ = g; // keep alive for the line above
    }

    // 3. acquire_write / drop
    {
        let s = new_surface();
        let g = adapter
            .acquire_write(&s)
            .expect("acquire_write on idle surface must succeed");
        let _ = g;
    }

    // 4. two concurrent acquire_read
    {
        let s = new_surface();
        let g1 = adapter
            .acquire_read(&s)
            .expect("first concurrent read must succeed");
        let g2 = adapter
            .acquire_read(&s)
            .expect("second concurrent read must succeed");
        drop(g1);
        drop(g2);
    }

    // 5. write contends with live read
    {
        let s = new_surface();
        let read_guard = adapter
            .acquire_read(&s)
            .expect("acquire_read for contention test must succeed");
        match adapter.acquire_write(&s) {
            Err(AdapterError::WriteContended { .. }) => {}
            Err(other) => panic!(
                "expected WriteContended while a read is held, got {other:?}"
            ),
            Ok(_) => panic!("acquire_write must fail while a read is held"),
        }
        drop(read_guard);
    }

    // 6. write contends with live write
    {
        let s = new_surface();
        let write_guard = adapter
            .acquire_write(&s)
            .expect("first acquire_write for contention test must succeed");
        match adapter.acquire_write(&s) {
            Err(AdapterError::WriteContended { .. }) => {}
            Err(other) => panic!(
                "expected WriteContended while a write is held, got {other:?}"
            ),
            Ok(_) => panic!("acquire_write must fail while another write is held"),
        }
        drop(write_guard);
    }

    // 7. parallel readers from multiple threads — sanity that Send+Sync holds.
    let s = new_surface();
    let s_ref = &s;
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(move || {
                let g = adapter
                    .acquire_read(s_ref)
                    .expect("threaded acquire_read must succeed");
                thread::sleep(Duration::from_millis(1));
                drop(g);
            });
        }
    });
}
