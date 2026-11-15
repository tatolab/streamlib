// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn test_capability_newtypes_delegate_and_convert() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    // Limited-access delegates to the same underlying context.
    let limited = GpuContextLimitedAccess::new(gpu.clone());
    let full = limited.to_full_access();

    // Delegated accessor reaches the same RHI device. `device()` is
    // FullAccess-only after #324; Sandbox reaches the same underlying
    // context through `to_full_access()` (crate-internal) or
    // `escalate()` for user code.
    let device_ptr_gpu = Arc::as_ptr(gpu.device());
    let device_ptr_full = Arc::as_ptr(full.device());
    assert_eq!(device_ptr_gpu, device_ptr_full);

    println!("GpuContextLimitedAccess + GpuContextFullAccess delegation: OK");
}

#[test]
fn test_escalate_serializes_concurrent_callers() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    const THREADS: usize = 8;
    let in_closure = Arc::new(AtomicBool::new(false));
    let overlap_count = Arc::new(AtomicUsize::new(0));
    let completed_count = Arc::new(AtomicUsize::new(0));

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let gpu = gpu.clone();
            let in_closure = Arc::clone(&in_closure);
            let overlap_count = Arc::clone(&overlap_count);
            let completed_count = Arc::clone(&completed_count);
            std::thread::spawn(move || {
                let limited = GpuContextLimitedAccess::new(gpu);
                limited
                    .escalate(|_full| {
                        if in_closure.swap(true, Ordering::SeqCst) {
                            overlap_count.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        in_closure.store(false, Ordering::SeqCst);
                        completed_count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .expect("escalate closure should succeed");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread panicked");
    }

    assert_eq!(
        overlap_count.load(Ordering::SeqCst),
        0,
        "escalate closures overlapped — setup mutex not held"
    );
    assert_eq!(completed_count.load(Ordering::SeqCst), THREADS);

    println!("escalate serializes concurrent callers: OK");
}

#[test]
fn test_escalate_propagates_closure_error() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let limited = GpuContextLimitedAccess::new(gpu);
    let result: Result<()> =
        limited.escalate(|_full| Err(Error::Runtime("synthetic failure".to_string())));
    match result {
        Err(Error::Runtime(msg)) if msg == "synthetic failure" => {}
        other => panic!("expected synthetic Runtime error, got {other:?}"),
    }

    // Mutex must be released after the error — a second escalation should proceed.
    let after: Result<u32> = limited.escalate(|_full| Ok(7));
    assert_eq!(after.expect("escalate after error"), 7);

    println!("escalate propagates closure error + releases lock: OK");
}

/// In-process escalate panic recovery (#1006 scenario 2).
///
/// `escalate_gate.enter_scoped()` is an RAII guard whose Drop runs
/// even when the closure panics — `catch_unwind` at the test
/// boundary catches the panic, and the subsequent `escalate` call
/// must proceed (proves the gate was released by the Drop on the
/// panicking closure's stack frame).
///
/// Mental revert: changing `enter_scoped()`'s Drop to a no-op (or
/// switching back to a manual lock/unlock pair without the RAII
/// release) would leave the gate held forever; the post-panic
/// `escalate` call would block until test timeout.
#[test]
fn test_escalate_releases_gate_on_panic() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let limited = GpuContextLimitedAccess::new(gpu);

    // Inside `catch_unwind`, intentionally panic inside an
    // escalate closure.
    let unwind_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<()> = limited.escalate(|_full| -> Result<()> {
            panic!("synthetic in-process escalate panic");
        });
    }));
    assert!(
        unwind_result.is_err(),
        "the catch_unwind block must observe the panic"
    );

    // The next escalate must succeed — proves the gate released
    // even though the closure unwound.
    let after: Result<u32> = limited.escalate(|_full| Ok(11));
    assert_eq!(
        after.expect("escalate after panic must succeed"),
        11,
        "escalate gate must release on panic via Drop"
    );

    println!("escalate releases gate on panic via RAII Drop: OK");
}

/// LimitedAccess + FullAccess interleaving (#1006 scenario 5).
///
/// LimitedAccess ops route through the shared command-queue
/// mutex; FullAccess escalates through the separate
/// `EscalateGate`. Concurrent callers — thread A holds an
/// escalate mid-closure, thread B issues an `acquire_pixel_buffer`
/// Limited call — must both complete without deadlock.
/// Documented model: the two locks are independent and Limited
/// observes no partial-Full state.
///
/// Mental revert: collapsing both locks into one would let
/// thread B block on the escalate gate; the assertion that
/// Limited completes before the escalate releases would fail.
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — needs GPU + #[serial] discipline; \
              exercises Limited+Full lock interleaving"
)]
#[test]
fn test_limited_and_full_interleave_without_deadlock() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };

    let limited = GpuContextLimitedAccess::new(gpu);
    let limited_a = limited.clone();
    let limited_b = limited.clone();

    let escalate_in_progress = Arc::new(AtomicBool::new(false));
    let escalate_in_progress_a = Arc::clone(&escalate_in_progress);
    let limited_observed_during_escalate = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&limited_observed_during_escalate);

    let thread_a = std::thread::spawn(move || {
        limited_a
            .escalate(|_full| -> Result<()> {
                escalate_in_progress_a.store(true, Ordering::SeqCst);
                // Hold the gate for ~200ms; thread B must complete
                // its Limited acquire in this window.
                std::thread::sleep(Duration::from_millis(200));
                escalate_in_progress_a.store(false, Ordering::SeqCst);
                Ok(())
            })
            .expect("escalate must succeed")
    });

    // Wait until thread A has entered the escalate closure.
    let start = std::time::Instant::now();
    while !escalate_in_progress.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_secs(2) {
            panic!("thread A never entered the escalate closure within 2s");
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    // Thread B issues a Limited acquire while the escalate is
    // mid-closure. This must complete BEFORE thread A releases
    // the gate — proves the two locks are independent.
    let thread_b = std::thread::spawn(move || {
        use crate::core::rhi::PixelFormat;
        let result = limited_b.acquire_pixel_buffer(16, 16, PixelFormat::Rgba32);
        if result.is_ok() {
            observed.store(true, Ordering::SeqCst);
        }
        result.map(|_| ())
    });

    thread_b.join().expect("thread B panicked").ok();

    // Thread B's Limited op completed; thread A's escalate may
    // still be in flight. If interleaving works, observed=true.
    assert!(
        limited_observed_during_escalate.load(Ordering::SeqCst),
        "thread B's Limited acquire failed — independent locks regression?"
    );

    thread_a.join().expect("thread A panicked");

    println!("Limited + Full interleave without deadlock: OK");
}

/// A node's limited tier and the full tier its escalate hands out count
/// one-offs against the one node, while the shared tier counts none.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn a_nodes_tiers_count_one_offs_against_the_node_while_the_shared_tier_counts_none() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };
    let layout =
        crate::core::rhi::TensorStorageBufferLayout::of_bytes(256).expect("a non-zero size");

    let node_tier = GpuContextLimitedAccess::counting_one_offs_against_the_node(
        gpu.clone(),
        Some(OneOffAcquisitionsCountedForOneNativeNode::new()),
    );
    let mut held_by_the_node = Vec::new();
    for _ in 0..8 {
        held_by_the_node.push(
            node_tier
                .acquire_storage_buffer(&layout)
                .expect("the node's limited tier acquires up to its count"),
        );
    }
    let held_through_the_escalate = node_tier
        .escalate(|full| {
            (0..8)
                .map(|_| full.acquire_storage_buffer(&layout))
                .collect::<Result<Vec<_>>>()
        })
        .expect("the escalate's full tier acquires up to the same count");
    held_by_the_node.extend(held_through_the_escalate);

    let refusal = node_tier
        .clone()
        .acquire_storage_buffer(&layout)
        .expect_err("a clone of the tier counts against the same node");
    assert!(
        matches!(
            refusal,
            Error::EveryOneOffAcquisitionOfThisShapeTheNodeMayHoldIsHeld { .. }
        ),
        "the node's seventeenth is refused by its own count: {refusal:?}"
    );

    held_by_the_node.pop();
    let _in_the_room_handed_back = node_tier
        .acquire_storage_buffer(&layout)
        .expect("a dropped one-off hands its count back");

    let shared_tier = GpuContextLimitedAccess::new(gpu);
    let _held_by_no_node: Vec<_> = (0..17)
        .map(|_| {
            shared_tier
                .acquire_storage_buffer(&layout)
                .expect("the shared tier counts no one-offs")
        })
        .collect();
}

/// A node's output texture stays registered and counted while its handle
/// lives, and dropping the handle unregisters its id and hands the count
/// back, so a node acquiring one every frame cannot grow the cache.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
fn dropping_a_nodes_output_texture_unregisters_its_id_and_hands_its_count_back() {
    let gpu = match GpuContext::init_for_platform() {
        Ok(g) => g,
        Err(_) => {
            println!("Skipping - no GPU device available");
            return;
        }
    };
    let one_offs_of_the_node = OneOffAcquisitionsCountedForOneNativeNode::new();
    let node_tier = GpuContextLimitedAccess::counting_one_offs_against_the_node(
        gpu.clone(),
        Some(Arc::clone(&one_offs_of_the_node)),
    );
    let output_texture_shape = OneOffAcquisitionShape::Texture {
        width: 64,
        height: 64,
        format: TextureFormat::Rgba8Unorm,
        usage: output_texture_usages(),
    };
    let is_registered = |id: &str| {
        gpu.texture_cache
            .lock()
            .unwrap()
            .contains_key(pool_slot_key_of_surface_id(id))
    };

    let (registered_under_id, output_texture) = node_tier
        .escalate(|full| full.acquire_output_texture(64, 64, TextureFormat::Rgba8Unorm))
        .expect("the node acquires an output texture");
    assert!(is_registered(&registered_under_id));
    assert_eq!(
        one_offs_of_the_node.held_count_of_shape(&output_texture_shape),
        1
    );

    drop(output_texture);
    assert!(
        !is_registered(&registered_under_id),
        "dropping the handle unregisters its id"
    );
    assert_eq!(
        one_offs_of_the_node.held_count_of_shape(&output_texture_shape),
        0,
        "dropping the handle hands its count back"
    );
}
