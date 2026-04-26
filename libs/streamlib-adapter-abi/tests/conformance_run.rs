// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Run the public conformance fixture against the reference
//! [`MockAdapter`] — proves the fixture is internally consistent and
//! gives 3rd-party adapter authors a working template to copy.

use streamlib_adapter_abi::testing::{empty_surface, run_conformance, MockAdapter};
use streamlib_adapter_abi::SurfaceAdapter;

#[test]
fn mock_adapter_passes_conformance_suite() {
    let adapter = MockAdapter::new();
    run_conformance(&adapter, empty_surface);

    let snap = adapter.snapshot();
    assert_eq!(
        snap.read_holders, 0,
        "every conformance read must release on drop, got {snap:?}",
    );
    assert!(
        !snap.write_held,
        "every conformance write must release on drop, got {snap:?}",
    );
    assert_eq!(
        snap.acquires_read, snap.releases_read,
        "read acquire/release must balance, got {snap:?}",
    );
    assert_eq!(
        snap.acquires_write, snap.releases_write,
        "write acquire/release must balance, got {snap:?}",
    );
}

#[test]
fn mock_adapter_surface_not_found_path() {
    let adapter = MockAdapter::new();
    adapter
        .fail_with_not_found
        .store(1, std::sync::atomic::Ordering::Release);
    let s = empty_surface(42);
    match adapter.acquire_read(&s) {
        Err(streamlib_adapter_abi::AdapterError::SurfaceNotFound { surface_id }) => {
            assert_eq!(surface_id, 42);
        }
        other => panic!("expected SurfaceNotFound, got {other:?}"),
    }
}
