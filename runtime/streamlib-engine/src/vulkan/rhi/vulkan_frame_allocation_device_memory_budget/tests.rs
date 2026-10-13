// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::*;

const BYTES_PER_MEBIBYTE: u64 = 1024 * 1024;
/// A device limit far from anything these readings hold.
const ROOMY_MAX_MEMORY_ALLOCATION_COUNT: u32 = u32::MAX;

fn frame_request(requested_bytes: u64) -> FrameDeviceMemoryRequest {
    FrameDeviceMemoryRequest {
        requested_bytes,
        placement: FrameDeviceMemoryPlacement::VulkanMemoryType {
            memory_type_index: 3,
        },
        frame_allocation_site: "a frame allocation in a test",
    }
}

fn heap_reading(
    heap_index: u32,
    heap_budget_bytes: u64,
    heap_usage_bytes: u64,
) -> DeviceMemoryHeapBudgetReading {
    DeviceMemoryHeapBudgetReading {
        heap_index,
        heap_locality: DeviceMemoryHeapLocality::DeviceLocal,
        heap_budget_bytes,
        heap_usage_bytes,
        bytes_this_runtimes_allocator_pools_hold_on_the_heap: heap_usage_bytes / 2,
        device_memory_blocks_this_runtimes_allocator_holds_on_the_heap: 12,
        heap_budget_source: DeviceMemoryHeapBudgetSource::ReportedByTheDriver,
    }
}

fn reading_of_one_heap(
    heap: DeviceMemoryHeapBudgetReading,
    live_device_memory_allocation_count: u64,
    max_memory_allocation_count: u32,
) -> FrameAllocationDeviceMemoryReading {
    FrameAllocationDeviceMemoryReading {
        budget: FrameAllocationBudgetReading::VulkanMemoryHeap(heap),
        allocation_count: DeviceMemoryAllocationCountReading {
            live_device_memory_allocation_count,
            max_memory_allocation_count,
        },
    }
}

fn roomy_reading_of_one_heap(
    heap_budget_bytes: u64,
    heap_usage_bytes: u64,
) -> FrameAllocationDeviceMemoryReading {
    reading_of_one_heap(
        heap_reading(0, heap_budget_bytes, heap_usage_bytes),
        16,
        ROOMY_MAX_MEMORY_ALLOCATION_COUNT,
    )
}

/// The plan's rule: a frame past its heap's budget is refused before anything
/// is allocated, on the numbers the reader fetched for that very allocation,
/// and the refusal names the heap, its budget, its usage and the pools' bytes.
#[test]
fn a_frame_allocation_past_the_heap_budget_is_refused_naming_the_heap() {
    let gate = FrameAllocationDeviceMemoryBudgetGate::new();
    let fetches_made = Cell::new(0_u32);
    // Each fetch answers different numbers, so a refusal carrying the second
    // fetch's numbers proves the second allocation was decided on its own fetch.
    let read_heap_one = || {
        fetches_made.set(fetches_made.get() + 1);
        Ok(match fetches_made.get() {
            1 => reading_of_one_heap(
                heap_reading(1, 2_048 * BYTES_PER_MEBIBYTE, 512 * BYTES_PER_MEBIBYTE),
                40,
                ROOMY_MAX_MEMORY_ALLOCATION_COUNT,
            ),
            _ => reading_of_one_heap(
                DeviceMemoryHeapBudgetReading {
                    heap_index: 1,
                    heap_locality: DeviceMemoryHeapLocality::DeviceLocal,
                    heap_budget_bytes: 1_900 * BYTES_PER_MEBIBYTE,
                    heap_usage_bytes: 1_850 * BYTES_PER_MEBIBYTE,
                    bytes_this_runtimes_allocator_pools_hold_on_the_heap: 700 * BYTES_PER_MEBIBYTE,
                    device_memory_blocks_this_runtimes_allocator_holds_on_the_heap: 41,
                    heap_budget_source: DeviceMemoryHeapBudgetSource::ReportedByTheDriver,
                },
                41,
                ROOMY_MAX_MEMORY_ALLOCATION_COUNT,
            ),
        })
    };

    let admitted = gate.allocate_a_frame_within_the_device_memory_budget(
        &frame_request(8 * BYTES_PER_MEBIBYTE),
        read_heap_one,
        || Ok("the first frame"),
    );
    assert_eq!(
        admitted.expect("room for the first frame"),
        "the first frame"
    );
    assert_eq!(fetches_made.get(), 1, "the first allocation fetched once");

    let allocate_the_frame_ran = Cell::new(false);
    let refusal = gate
        .allocate_a_frame_within_the_device_memory_budget(
            &frame_request(8 * BYTES_PER_MEBIBYTE),
            read_heap_one,
            || {
                allocate_the_frame_ran.set(true);
                Ok("the second frame")
            },
        )
        .expect_err("the heap has less room than the frame and the headroom");

    assert!(
        !allocate_the_frame_ran.get(),
        "nothing is allocated past the budget"
    );
    assert_eq!(
        fetches_made.get(),
        2,
        "the second allocation fetched a reading of its own before refusing"
    );
    match &refusal {
        Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
            heap_index,
            heap_budget_bytes,
            heap_usage_bytes,
            bytes_this_runtimes_allocator_pools_hold_on_the_heap,
            requested_bytes,
            headroom_bytes,
            frame_allocation_site,
            ..
        } => {
            assert_eq!(*heap_index, 1);
            assert_eq!(*heap_budget_bytes, 1_900 * BYTES_PER_MEBIBYTE);
            assert_eq!(*heap_usage_bytes, 1_850 * BYTES_PER_MEBIBYTE);
            assert_eq!(
                *bytes_this_runtimes_allocator_pools_hold_on_the_heap,
                700 * BYTES_PER_MEBIBYTE
            );
            assert_eq!(*requested_bytes, 8 * BYTES_PER_MEBIBYTE);
            assert_eq!(
                *headroom_bytes,
                HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET
            );
            assert_eq!(frame_allocation_site, "a frame allocation in a test");
        }
        other => panic!("expected the heap-budget refusal, got {other:?}"),
    }
    let message = refusal.to_string();
    for named in [
        "device memory heap 1".to_string(),
        format!("budget of {} bytes", 1_900 * BYTES_PER_MEBIBYTE),
        format!("uses {} bytes", 1_850 * BYTES_PER_MEBIBYTE),
        format!("pools hold {} bytes", 700 * BYTES_PER_MEBIBYTE),
        format!("a {}-byte frame allocation", 8 * BYTES_PER_MEBIBYTE),
        format!(
            "the {HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET}-byte headroom"
        ),
    ] {
        assert!(
            message.contains(&named),
            "the refusal names `{named}`: {message}"
        );
    }
}

#[test]
fn a_frame_landing_exactly_on_the_budget_less_the_headroom_is_admitted() {
    let budget = 4_096 * BYTES_PER_MEBIBYTE;
    let request = 16 * BYTES_PER_MEBIBYTE;
    let usage =
        budget - HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET - request;
    assert!(
        refusal_of_a_frame_allocation_past_the_device_memory_bound(
            &frame_request(request),
            &roomy_reading_of_one_heap(budget, usage),
        )
        .is_none(),
        "a frame that leaves exactly the headroom fits"
    );
}

#[test]
fn a_frame_one_byte_past_the_budget_less_the_headroom_is_refused() {
    let budget = 4_096 * BYTES_PER_MEBIBYTE;
    let request = 16 * BYTES_PER_MEBIBYTE;
    let usage =
        budget - HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET - request
            + 1;
    assert!(matches!(
        refusal_of_a_frame_allocation_past_the_device_memory_bound(
            &frame_request(request),
            &roomy_reading_of_one_heap(budget, usage),
        ),
        Some(Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget { .. })
    ));
}

#[test]
fn a_heap_whose_budget_is_below_the_headroom_refuses_every_frame() {
    let budget_below_the_headroom =
        HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET - 1;
    for request in [1, 4_096, BYTES_PER_MEBIBYTE] {
        assert!(
            matches!(
                refusal_of_a_frame_allocation_past_the_device_memory_bound(
                    &frame_request(request),
                    &roomy_reading_of_one_heap(budget_below_the_headroom, 0),
                ),
                Some(Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget { .. })
            ),
            "a {request}-byte frame on a heap with no room past the headroom is refused"
        );
    }
}

#[test]
fn fewer_than_the_reserved_device_memory_allocations_left_refuses_naming_the_device_limit() {
    let max_memory_allocation_count = 4_096;
    let live = u64::from(
        max_memory_allocation_count
            - DEVICE_MEMORY_ALLOCATIONS_KEPT_IN_RESERVE_UNDER_THE_DEVICE_LIMIT,
    ) + 1;
    let refusal = refusal_of_a_frame_allocation_past_the_device_memory_bound(
        &frame_request(BYTES_PER_MEBIBYTE),
        &reading_of_one_heap(
            heap_reading(0, 4_096 * BYTES_PER_MEBIBYTE, 0),
            live,
            max_memory_allocation_count,
        ),
    )
    .expect("127 allocations left under the limit is past the reserve");
    match &refusal {
        Error::FrameAllocationRefusedNearTheDeviceMemoryAllocationCountLimit {
            live_device_memory_allocation_count,
            max_memory_allocation_count: refused_against,
            allocations_kept_in_reserve,
            ..
        } => {
            assert_eq!(*live_device_memory_allocation_count, live);
            assert_eq!(*refused_against, max_memory_allocation_count);
            assert_eq!(
                *allocations_kept_in_reserve,
                DEVICE_MEMORY_ALLOCATIONS_KEPT_IN_RESERVE_UNDER_THE_DEVICE_LIMIT
            );
        }
        other => panic!("expected the allocation-count refusal, got {other:?}"),
    }
    assert!(
        refusal
            .to_string()
            .contains("of the 4096 the device allows"),
        "{refusal}"
    );
}

#[test]
fn exactly_the_reserved_device_memory_allocations_left_is_still_admitted() {
    let max_memory_allocation_count = 4_096;
    let live = u64::from(
        max_memory_allocation_count
            - DEVICE_MEMORY_ALLOCATIONS_KEPT_IN_RESERVE_UNDER_THE_DEVICE_LIMIT,
    );
    assert!(
        refusal_of_a_frame_allocation_past_the_device_memory_bound(
            &frame_request(BYTES_PER_MEBIBYTE),
            &reading_of_one_heap(
                heap_reading(0, 4_096 * BYTES_PER_MEBIBYTE, 0),
                live,
                max_memory_allocation_count,
            ),
        )
        .is_none()
    );
}

#[test]
fn a_frame_past_both_bounds_is_refused_naming_the_heap_budget_first() {
    let refusal = refusal_of_a_frame_allocation_past_the_device_memory_bound(
        &frame_request(BYTES_PER_MEBIBYTE),
        &reading_of_one_heap(
            heap_reading(
                0,
                HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET,
                HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET,
            ),
            4_096,
            4_096,
        ),
    );
    assert!(matches!(
        refusal,
        Some(Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget { .. })
    ));
}

#[test]
fn the_device_memory_allocation_count_is_allocator_blocks_plus_raw_allocations() {
    let every_heaps_reading = [
        heap_reading(0, 0, 0),
        DeviceMemoryHeapBudgetReading {
            device_memory_blocks_this_runtimes_allocator_holds_on_the_heap: 30,
            ..heap_reading(1, 0, 0)
        },
    ];
    assert_eq!(
        device_memory_allocations_this_process_holds(
            every_heaps_reading
                .iter()
                .map(|heap| heap.device_memory_blocks_this_runtimes_allocator_holds_on_the_heap),
            5,
        ),
        12 + 30 + 5
    );
    assert_eq!(device_memory_allocations_this_process_holds([], 0), 0);
}

#[test]
fn without_the_budget_extension_a_host_heaps_room_is_capped_by_available_system_memory() {
    let eighty_percent_of_the_heap = 25_600 * BYTES_PER_MEBIBYTE;
    let usage = 1_000 * BYTES_PER_MEBIBYTE;
    let system_memory_available = 3_000 * BYTES_PER_MEBIBYTE;
    assert_eq!(
        heap_budget_without_the_memory_budget_extension(
            eighty_percent_of_the_heap,
            usage,
            DeviceMemoryHeapLocality::NotDeviceLocal,
            Some(system_memory_available),
        ),
        (
            usage + system_memory_available,
            DeviceMemoryHeapBudgetSource::EstimatedAndCappedByTheSystemsAvailableMemory
        ),
        "the room is the system's available memory, not the heap's usage counted twice"
    );
    assert_eq!(
        heap_budget_without_the_memory_budget_extension(
            eighty_percent_of_the_heap,
            usage,
            DeviceMemoryHeapLocality::NotDeviceLocal,
            Some(64_000 * BYTES_PER_MEBIBYTE),
        ),
        (
            eighty_percent_of_the_heap,
            DeviceMemoryHeapBudgetSource::EstimatedAsEightyPercentOfTheHeap
        ),
        "a cap above the estimate leaves the estimate"
    );
    assert_eq!(
        heap_budget_without_the_memory_budget_extension(
            eighty_percent_of_the_heap,
            usage,
            DeviceMemoryHeapLocality::NotDeviceLocal,
            None,
        ),
        (
            eighty_percent_of_the_heap,
            DeviceMemoryHeapBudgetSource::EstimatedAsEightyPercentOfTheHeap
        ),
        "an unreadable available memory leaves the estimate"
    );
}

#[test]
fn without_the_budget_extension_a_device_local_heap_ignores_available_system_memory() {
    let eighty_percent_of_the_heap = 19_660 * BYTES_PER_MEBIBYTE;
    assert_eq!(
        heap_budget_without_the_memory_budget_extension(
            eighty_percent_of_the_heap,
            100 * BYTES_PER_MEBIBYTE,
            DeviceMemoryHeapLocality::DeviceLocal,
            Some(BYTES_PER_MEBIBYTE),
        ),
        (
            eighty_percent_of_the_heap,
            DeviceMemoryHeapBudgetSource::EstimatedAsEightyPercentOfTheHeap
        )
    );
}

#[test]
fn the_metal_working_set_refuses_past_the_recommended_maximum_less_the_headroom_naming_its_usage() {
    let recommended_max_working_set = 10_240 * BYTES_PER_MEBIBYTE;
    let request = 16 * BYTES_PER_MEBIBYTE;
    let reading_with_metal_current_allocated =
        |metal_current_allocated_bytes: u64| FrameAllocationDeviceMemoryReading {
            budget: FrameAllocationBudgetReading::MetalWorkingSet(MetalWorkingSetBudgetReading {
                recommended_max_working_set_bytes: recommended_max_working_set,
                metal_current_allocated_bytes,
            }),
            allocation_count: DeviceMemoryAllocationCountReading {
                live_device_memory_allocation_count: 0,
                max_memory_allocation_count: ROOMY_MAX_MEMORY_ALLOCATION_COUNT,
            },
        };
    let metal_current_allocated_that_fills_the_working_set = recommended_max_working_set
        - HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET
        - request;

    assert!(
        refusal_of_a_frame_allocation_past_the_device_memory_bound(
            &frame_request(request),
            &reading_with_metal_current_allocated(
                metal_current_allocated_that_fills_the_working_set
            ),
        )
        .is_none(),
        "the working set's last room is still room"
    );
    let refusal = refusal_of_a_frame_allocation_past_the_device_memory_bound(
        &frame_request(request),
        &reading_with_metal_current_allocated(
            metal_current_allocated_that_fills_the_working_set + 1,
        ),
    )
    .expect("one allocated byte more leaves less than the headroom");
    match &refusal {
        Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget {
            recommended_max_working_set_bytes,
            metal_current_allocated_bytes,
            requested_bytes,
            headroom_bytes,
            ..
        } => {
            assert_eq!(
                *recommended_max_working_set_bytes,
                recommended_max_working_set
            );
            assert_eq!(
                *metal_current_allocated_bytes,
                metal_current_allocated_that_fills_the_working_set + 1
            );
            assert_eq!(*requested_bytes, request);
            assert_eq!(
                *headroom_bytes,
                HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET
            );
        }
        other => panic!("expected the Metal working-set refusal, got {other:?}"),
    }
}

#[test]
fn a_budget_left_with_no_room_by_a_test_refuses_every_frame() {
    let gate = FrameAllocationDeviceMemoryBudgetGate::new();
    gate.leave_no_room_in_any_budget_for_this_test();
    for reading in [
        roomy_reading_of_one_heap(64_000 * BYTES_PER_MEBIBYTE, 0),
        roomy_reading_of_one_heap(64_000 * BYTES_PER_MEBIBYTE, 32_000 * BYTES_PER_MEBIBYTE),
        FrameAllocationDeviceMemoryReading {
            budget: FrameAllocationBudgetReading::MetalWorkingSet(MetalWorkingSetBudgetReading {
                recommended_max_working_set_bytes: 64_000 * BYTES_PER_MEBIBYTE,
                metal_current_allocated_bytes: 0,
            }),
            allocation_count: DeviceMemoryAllocationCountReading {
                live_device_memory_allocation_count: 0,
                max_memory_allocation_count: ROOMY_MAX_MEMORY_ALLOCATION_COUNT,
            },
        },
    ] {
        let allocated = gate.allocate_a_frame_within_the_device_memory_budget(
            &frame_request(1),
            || Ok(reading.clone()),
            || Ok(()),
        );
        assert!(
            allocated.is_err(),
            "a budget lowered to its usage has no room: {reading:?}"
        );
    }
}

#[test]
fn a_device_memory_reading_that_fails_allocates_nothing() {
    let gate = FrameAllocationDeviceMemoryBudgetGate::new();
    let allocate_the_frame_ran = Cell::new(false);
    let refusal = gate
        .allocate_a_frame_within_the_device_memory_budget(
            &frame_request(BYTES_PER_MEBIBYTE),
            || Err(Error::GpuError("the reading failed".to_string())),
            || {
                allocate_the_frame_ran.set(true);
                Ok(())
            },
        )
        .expect_err("no reading, no allocation");
    assert!(!allocate_the_frame_ran.get());
    assert!(
        refusal.to_string().contains("the reading failed"),
        "{refusal}"
    );
}

/// The check and the allocation are one step: while one frame allocation sits
/// inside its allocation, a second waits for it rather than deciding on the
/// reading the first is about to change.
#[test]
fn two_frame_allocations_racing_for_the_last_room_never_both_pass() {
    let budget = 1_024 * BYTES_PER_MEBIBYTE;
    let request = 32 * BYTES_PER_MEBIBYTE;
    let heap_usage = Arc::new(AtomicU64::new(
        budget - HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET - request,
    ));
    let gate = Arc::new(FrameAllocationDeviceMemoryBudgetGate::new());
    let reading_of_the_shared_heap = move |heap_usage: &AtomicU64| {
        roomy_reading_of_one_heap(budget, heap_usage.load(Ordering::SeqCst))
    };

    let (first_is_inside_its_allocation_tx, first_is_inside_its_allocation_rx) = mpsc::channel();
    let (let_the_first_finish_tx, let_the_first_finish_rx) = mpsc::channel::<()>();
    let first = {
        let gate = Arc::clone(&gate);
        let heap_usage = Arc::clone(&heap_usage);
        std::thread::spawn(move || {
            gate.allocate_a_frame_within_the_device_memory_budget(
                &frame_request(request),
                || Ok(reading_of_the_shared_heap(&heap_usage)),
                || {
                    first_is_inside_its_allocation_tx
                        .send(())
                        .expect("the test is listening");
                    let_the_first_finish_rx
                        .recv_timeout(Duration::from_secs(10))
                        .expect("the test lets the first allocation finish");
                    heap_usage.fetch_add(request, Ordering::SeqCst);
                    Ok("first")
                },
            )
        })
    };
    first_is_inside_its_allocation_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the first allocation reaches its allocation");

    let (second_verdict_tx, second_verdict_rx) = mpsc::channel();
    let second = {
        let gate = Arc::clone(&gate);
        let heap_usage = Arc::clone(&heap_usage);
        std::thread::spawn(move || {
            let verdict = gate.allocate_a_frame_within_the_device_memory_budget(
                &frame_request(request),
                || Ok(reading_of_the_shared_heap(&heap_usage)),
                || {
                    heap_usage.fetch_add(request, Ordering::SeqCst);
                    Ok("second")
                },
            );
            second_verdict_tx
                .send(verdict.is_ok())
                .expect("the test is listening");
        })
    };
    let verdict_while_the_first_was_allocating =
        second_verdict_rx.recv_timeout(Duration::from_millis(200));
    let_the_first_finish_tx
        .send(())
        .expect("the first allocation is waiting");

    let first_passed = first.join().expect("the first allocation's thread").is_ok();
    let second_passed = match verdict_while_the_first_was_allocating {
        Ok(passed) => passed,
        Err(_) => second_verdict_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the second allocation decides once the first is done"),
    };
    second.join().expect("the second allocation's thread");
    assert!(first_passed, "the first allocation had the room");
    assert!(
        !second_passed,
        "only one allocation fits the last room, so the second is refused"
    );
    assert_eq!(
        heap_usage.load(Ordering::SeqCst),
        budget - HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET
    );
}
