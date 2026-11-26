// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The frame thread's flush and reads allocate nothing, counted by an allocator that counts the
//! allocations of the thread that asks. Its own test binary, since it replaces the allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use streamlib_scope::{ScopeBeingDeclared, computed};

struct CountingOnOneThread;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count_if_counting() {
    if COUNTING.get() {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
    }
}

// SAFETY: every call forwards to `System` unchanged; counting touches only `const` thread
// locals, which neither allocate nor reenter the allocator.
unsafe impl GlobalAlloc for CountingOnOneThread {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_if_counting();
        // SAFETY: the caller's contract for `alloc` is `System::alloc`'s.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_if_counting();
        // SAFETY: the caller's contract for `alloc_zeroed` is `System::alloc_zeroed`'s.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_if_counting();
        // SAFETY: the caller's contract for `realloc` is `System::realloc`'s.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc` is `System::dealloc`'s.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingOnOneThread = CountingOnOneThread;

/// The allocations this thread makes in `counted`.
fn allocations_in<R>(counted: impl FnOnce() -> R) -> (R, usize) {
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    let result = counted();
    COUNTING.set(false);
    (result, ALLOCATIONS.get())
}

#[test]
fn flush_and_get_allocate_nothing_while_inputs_change_and_values_rebuild() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(1_u32);
    let radius = declared.input(1.0_f32);
    let kernel = declared
        .declare(|| computed(move || Ok::<_, String>(vec![0_u8; 64 * *taps.get() as usize])));
    let (scope, mut view) = declared.start().unwrap();
    // Warm the frame thread's thread locals before counting.
    let deadline = Instant::now() + Duration::from_secs(5);
    while view.run_frame(|| ()).is_none() {
        assert!(
            Instant::now() < deadline,
            "the first kernel was never built"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut last_kernel_len = 0;
    let mut measured_frame = |step: u32| -> usize {
        let (kernel_len, allocations) = allocations_in(|| {
            view.flush();
            view.run_frame(|| {
                let _ = *radius.get();
                kernel.get().len()
            })
        });
        assert_eq!(allocations, 0, "frame {step} allocated");
        kernel_len.expect("a built scope runs every frame")
    };
    for step in 1..=2_000_u32 {
        scope.write(|writes| {
            writes.write(taps, 1 + step % 7);
            writes.write(radius, step as f32);
        });
        last_kernel_len = measured_frame(step);
        if step % 100 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    // Then one kernel the frames have not seen: the counted frames include the one that takes it.
    let unseen_taps = 8 + last_kernel_len as u32 / 64;
    scope.write(|writes| writes.write(taps, unseen_taps));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut step = 2_001;
    while measured_frame(step) != 64 * unseen_taps as usize {
        assert!(
            Instant::now() < deadline,
            "the unseen kernel never reached a frame"
        );
        std::thread::sleep(Duration::from_millis(1));
        step += 1;
    }
}
