// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Blur taps, a compositor gaining a layer and an exclusive device held as a computed value, run
//! against the scope with a frame loop standing in for a node's per-frame function.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

mod common;

use common::Counted;
use streamlib_scope::{BuildFailure, FrameView, LastBuild, ScopeBeingDeclared, computed};

/// A kernel built for a tap count, standing in for a compiled blur shader.
#[derive(Debug, PartialEq)]
struct BlurKernel {
    taps: u32,
}

const SETTLE: Duration = Duration::from_secs(5);

fn run_frames_until(view: &mut FrameView, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if view.run_frame(&mut done) == Some(true) {
            return;
        }
        assert!(Instant::now() < deadline, "the frame loop waited too long");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Opens a gate builds wait on when dropped, so a failing test never leaves the worker blocked
/// while the scope joins it.
struct OpenOnDrop(Arc<AtomicBool>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn blur_keeps_its_old_kernel_until_the_new_one_is_built_and_ignores_the_radius() {
    let builds = Arc::new(AtomicUsize::new(0));
    let builds_may_finish = Arc::new(AtomicBool::new(false));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(5_u32);
    let radius = declared.input(1.0_f32);
    let kernel = declared.declare(|| {
        let builds = Arc::clone(&builds);
        let builds_may_finish = Arc::clone(&builds_may_finish);
        computed(move || {
            let taps = *taps.get();
            while !builds_may_finish.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            builds.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    let _open_on_unwind = OpenOnDrop(Arc::clone(&builds_may_finish));

    // No frame runs until the kernel has its first value.
    let before_the_first_value = view.run_frame(|| ());
    builds_may_finish.store(true, Ordering::SeqCst);
    assert_eq!(before_the_first_value, None);
    run_frames_until(&mut view, || true);
    assert_eq!(view.run_frame(|| kernel.get().taps), Some(5));

    // A new tap count: frames keep the 5-tap kernel while the 9-tap one builds.
    builds_may_finish.store(false, Ordering::SeqCst);
    scope.write(|writes| writes.write(taps, 9));
    // A flush that finds the hand-over held takes the 9 at a later frame.
    let mut while_building = None;
    run_frames_until(&mut view, || {
        let seen = (*taps.get(), kernel.get().taps);
        while_building = Some(seen);
        seen.0 == 9
    });
    builds_may_finish.store(true, Ordering::SeqCst);
    assert_eq!(while_building, Some((9, 5)));
    run_frames_until(&mut view, || kernel.get().taps == 9);

    // The kernel never read the radius, so a radius drag builds nothing.
    let before = builds.load(Ordering::SeqCst);
    for step in 0..50 {
        scope.write(|writes| writes.write(radius, step as f32));
    }
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(builds.load(Ordering::SeqCst), before);
}

#[test]
fn a_burst_of_tap_counts_ends_on_the_newest_and_throws_stale_builds_away() {
    let builds = Arc::new(AtomicUsize::new(0));
    let builds_may_finish = Arc::new(AtomicBool::new(true));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(1_u32);
    let kernel = declared.declare(|| {
        let builds = Arc::clone(&builds);
        let builds_may_finish = Arc::clone(&builds_may_finish);
        computed(move || {
            let taps = *taps.get();
            while !builds_may_finish.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            builds.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    let _open_on_unwind = OpenOnDrop(Arc::clone(&builds_may_finish));
    run_frames_until(&mut view, || true);

    // Builds are held for the whole burst, so at most one starts during it.
    builds_may_finish.store(false, Ordering::SeqCst);
    let mut handed_over = Vec::new();
    for taps_now in 2..=30 {
        scope.write(|writes| writes.write(taps, taps_now));
        handed_over.extend(view.run_frame(|| kernel.get().taps));
    }
    builds_may_finish.store(true, Ordering::SeqCst);
    run_frames_until(&mut view, || kernel.get().taps == 30);
    assert!(scope.wait_until_settled(SETTLE));

    // The first build, one held build read mid-burst, and one from the newest value.
    let builds = builds.load(Ordering::SeqCst);
    assert!(builds <= 3, "builds: {builds}");
    assert!(
        handed_over.windows(2).all(|pair| pair[0] <= pair[1]),
        "{handed_over:?}"
    );
}

#[test]
fn a_build_whose_input_changed_while_it_ran_is_thrown_away() {
    let (read_tx, read_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first_build = Mutex::new(Some((read_tx, release_rx)));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(1_u32);
    let kernel = declared.declare(|| {
        computed(move || {
            let taps = *taps.get();
            if let Some((read_tx, release_rx)) = first_build.lock().unwrap().take() {
                read_tx.send(()).unwrap();
                let _released_or_dropped = release_rx.recv();
            }
            Ok::<_, String>(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    // Bound after the scope, so it drops first and frees the build if the test unwinds.
    let release_tx = release_tx;

    // The build has read 1; 2 is written before it returns.
    read_rx
        .recv_timeout(SETTLE)
        .expect("the first build reads its input");
    scope.write(|writes| writes.write(taps, 2));
    drop(release_tx);

    run_frames_until(&mut view, || kernel.get().taps == 2);
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(scope.last_build_of(kernel), LastBuild::Built { builds: 1 });
}

/// Stops a background writer when dropped, so a failing test never leaves it running.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn a_value_whose_input_changes_faster_than_it_builds_does_not_starve_the_others() {
    let mut declared = ScopeBeingDeclared::new();
    let dragged = declared.input(0_u32);
    let other = declared.input(0_u32);
    let (_slow, follows_other) = declared.declare(|| {
        let slow = computed(move || {
            let dragged = *dragged.get();
            std::thread::sleep(Duration::from_millis(20));
            Ok::<_, String>(dragged)
        });
        let follows_other = computed(move || Ok::<_, String>(*other.get()));
        (slow, follows_other)
    });
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || true);

    let scope = Arc::new(scope);
    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicUsize::new(0));
    let writer = {
        let (scope, stop, writes) = (Arc::clone(&scope), Arc::clone(&stop), Arc::clone(&writes));
        std::thread::spawn(move || {
            let mut step = 1;
            while !stop.load(Ordering::SeqCst) {
                scope.write(|writes| writes.write(dragged, step));
                writes.fetch_add(1, Ordering::SeqCst);
                step += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };
    let stop_on_unwind = StopOnDrop(Arc::clone(&stop));
    while writes.load(Ordering::SeqCst) < 50 {
        std::thread::sleep(Duration::from_millis(1));
    }

    // While `dragged` keeps changing, the other value still builds.
    scope.write(|writes| writes.write(other, 1));
    run_frames_until(&mut view, || *follows_other.get() == 1);
    drop(stop_on_unwind);
    writer.join().unwrap();
}

#[test]
fn writes_no_frame_has_taken_keep_only_the_newest_value() {
    let alive = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let level = declared.input(Counted::new(0, &alive));
    let (scope, mut view) = declared.start().unwrap();

    for value in 1..=100 {
        scope.write(|writes| writes.write(level, Counted::new(value, &alive)));
    }
    // The frame view's value and the newest one, nothing in between.
    assert_eq!(alive.load(Ordering::SeqCst), 2);
    assert_eq!(view.run_frame(|| level.get().value), Some(100));
}

#[test]
fn writing_the_value_an_input_holds_builds_nothing() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let kernel = declared.declare(|| {
        let builds = Arc::clone(&builds);
        computed(move || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(BlurKernel { taps: *taps.get() })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || true);

    for _ in 0..10 {
        scope.write(|writes| writes.write(taps, 3));
    }
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(builds.load(Ordering::SeqCst), 1);
    assert_eq!(scope.last_build_of(kernel), LastBuild::Built { builds: 1 });
}

/// One layer of a compositor: what an opacity drag changes.
#[derive(Clone, Debug, PartialEq)]
struct Layer {
    opacity: f32,
}

#[test]
fn a_compositor_gains_a_layer_live_and_an_opacity_drag_builds_no_kernel() {
    let kernel_builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    // The slot list, an input the engine writes from the node's links.
    let layers = declared.input(vec![Layer { opacity: 1.0 }, Layer { opacity: 1.0 }]);
    let kernels = declared.declare(|| {
        let layer_count = computed(move || Ok::<_, String>(layers.get().len()));
        let kernel_builds = Arc::clone(&kernel_builds);
        computed(move || {
            let count = *layer_count.get();
            kernel_builds.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(
                (0..count)
                    .map(|_| BlurKernel { taps: 1 })
                    .collect::<Vec<_>>(),
            )
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || kernels.get().len() == 2);
    let after_first = kernel_builds.load(Ordering::SeqCst);

    // Drag the first layer's opacity: the whole slot list is written each time, the count
    // stays 2, so the kernel list is not built again.
    for step in 0..40 {
        scope.write(|writes| {
            writes.write(
                layers,
                vec![
                    Layer {
                        opacity: step as f32 / 40.0,
                    },
                    Layer { opacity: 1.0 },
                ],
            );
        });
    }
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(kernel_builds.load(Ordering::SeqCst), after_first);

    // Wire the empty slot: a third layer. The kernels follow without a rebuild of the node.
    scope.write(|writes| {
        writes.write(
            layers,
            vec![
                Layer { opacity: 1.0 },
                Layer { opacity: 1.0 },
                Layer { opacity: 0.5 },
            ],
        );
    });
    run_frames_until(&mut view, || kernels.get().len() == 3);
    assert_eq!(view.run_frame(|| layers.get().len()), Some(3));
}

/// A device only one holder can have, like `/dev/video0`. Opening it takes it from the holder
/// before, whose lease then reads nothing.
#[derive(Default)]
struct ExclusiveDevice {
    holder: Mutex<Option<Arc<Mutex<bool>>>>,
    open_holders: AtomicUsize,
}

struct DeviceLease {
    device: Arc<ExclusiveDevice>,
    held: Arc<Mutex<bool>>,
    device_id: u32,
}

impl PartialEq for DeviceLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.held, &other.held)
    }
}

impl ExclusiveDevice {
    fn open(self: &Arc<Self>, device_id: u32) -> DeviceLease {
        let mut holder = self.holder.lock().unwrap();
        if let Some(previous) = holder.take() {
            *previous.lock().unwrap() = false;
            self.open_holders.fetch_sub(1, Ordering::SeqCst);
        }
        let held = Arc::new(Mutex::new(true));
        *holder = Some(Arc::clone(&held));
        let now_open = self.open_holders.fetch_add(1, Ordering::SeqCst) + 1;
        assert_eq!(now_open, 1, "two holders of one exclusive device");
        DeviceLease {
            device: Arc::clone(self),
            held,
            device_id,
        }
    }
}

impl DeviceLease {
    fn frame(&self) -> Option<u32> {
        (*self.held.lock().unwrap()).then_some(self.device_id)
    }
}

impl Drop for DeviceLease {
    fn drop(&mut self) {
        let mut holder = self.device.holder.lock().unwrap();
        if holder
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, &self.held))
        {
            *holder = None;
            self.device.open_holders.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[test]
fn a_camera_device_change_opens_the_new_device_with_never_two_holders() {
    let device = Arc::new(ExclusiveDevice::default());
    let mut declared = ScopeBeingDeclared::new();
    let device_id = declared.input(0_u32);
    let camera = declared.declare(|| {
        let device = Arc::clone(&device);
        computed(move || Ok::<_, String>(device.open(*device_id.get())))
    });
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || camera.get().frame() == Some(0));

    scope.write(|writes| writes.write(device_id, 1));
    run_frames_until(&mut view, || camera.get().frame() == Some(1));
    // The replaced lease is released off the frame thread; one holder throughout.
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(device.open_holders.load(Ordering::SeqCst), 1);
}

/// Three taps of a kernel a transaction always sets to one value together.
#[derive(Debug, PartialEq)]
struct ThreeTaps {
    taps: [u32; 3],
}

#[test]
fn three_inputs_set_in_one_transaction_build_once() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let first = declared.input(0_u32);
    let second = declared.input(0_u32);
    let third = declared.input(0_u32);
    let kernel = declared.declare(|| {
        let builds = Arc::clone(&builds);
        computed(move || {
            builds.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(ThreeTaps {
                taps: [*first.get(), *second.get(), *third.get()],
            })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(builds.load(Ordering::SeqCst), 1);

    // A pause between the writes gives the worker time to build between them, were they
    // applied one by one.
    scope.write(|writes| {
        writes.write(first, 7);
        std::thread::sleep(Duration::from_millis(20));
        writes.write(second, 7);
        std::thread::sleep(Duration::from_millis(20));
        writes.write(third, 7);
    });
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(builds.load(Ordering::SeqCst), 2);
    run_frames_until(&mut view, || kernel.get().taps == [7, 7, 7]);
}

#[test]
fn no_build_or_frame_sees_a_transaction_half_applied() {
    let mut declared = ScopeBeingDeclared::new();
    let first = declared.input(0_u32);
    let second = declared.input(0_u32);
    let third = declared.input(0_u32);
    let kernel = declared.declare(|| {
        computed(move || {
            Ok::<_, String>(ThreeTaps {
                taps: [*first.get(), *second.get(), *third.get()],
            })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    let writing_done = Arc::new(AtomicBool::new(false));

    std::thread::scope(|threads| {
        let writer = threads.spawn(|| {
            let _done = OpenOnDrop(Arc::clone(&writing_done));
            for value in 1..=500_u32 {
                scope.write(|writes| {
                    writes.write(first, value);
                    writes.write(second, value);
                    writes.write(third, value);
                });
            }
        });
        while !writing_done.load(Ordering::SeqCst) {
            view.run_frame(|| {
                let inputs = [*first.get(), *second.get(), *third.get()];
                assert!(
                    inputs.iter().all(|tap| *tap == inputs[0]),
                    "a frame saw {inputs:?}"
                );
                let built = kernel.get().taps;
                assert!(
                    built.iter().all(|tap| *tap == built[0]),
                    "a build saw {built:?}"
                );
            });
        }
        writer.join().unwrap();
    });
    run_frames_until(&mut view, || kernel.get().taps == [500, 500, 500]);
}

#[test]
fn a_transaction_naming_another_scopes_input_is_refused_whole() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let mut other = ScopeBeingDeclared::new();
    let others_taps = other.input(3_u32);
    let (scope, mut view) = declared.start().unwrap();

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scope.write(|writes| {
            writes.write(taps, 9);
            writes.write(others_taps, 9);
        });
    }));
    assert!(refused.is_err());
    assert_eq!(view.run_frame(|| *taps.get()), Some(3));
}

#[test]
fn a_transaction_whose_refusal_is_caught_is_still_refused_whole() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let mut other = ScopeBeingDeclared::new();
    let others_taps = other.input(3_u32);
    let (scope, mut view) = declared.start().unwrap();

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scope.write(|writes| {
            writes.write(taps, 9);
            let _caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                writes.write(others_taps, 9);
            }));
        });
    }));
    assert!(refused.is_err());
    assert_eq!(view.run_frame(|| *taps.get()), Some(3));
}

#[test]
fn a_transaction_handed_another_scopes_writes_is_refused_whole() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let mut other = ScopeBeingDeclared::new();
    let others_taps = other.input(3_u32);
    let (scope, mut view) = declared.start().unwrap();
    let (other_scope, mut other_view) = other.start().unwrap();

    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scope.write(|writes| {
            other_scope.write(|others_writes| {
                others_writes.write(others_taps, 9);
                std::mem::swap(writes, others_writes);
            });
        });
    }));
    assert!(refused.is_err());
    assert_eq!(view.run_frame(|| *taps.get()), Some(3));
    assert_eq!(other_view.run_frame(|| *others_taps.get()), Some(3));
}

#[test]
fn closing_a_scope_with_builds_pending_returns_without_running_them() {
    let (first_build_started, started) = mpsc::channel();
    let (release_first_build, released) = mpsc::channel::<()>();
    let pending_builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let released = Mutex::new(released);
    let _in_flight = declared.declare(|| {
        computed(move || {
            let taps = *taps.get();
            first_build_started.send(()).unwrap();
            // Waits for the release, or for the sender to go if the test unwinds.
            let _ = released.lock().unwrap().recv();
            Ok::<_, String>(BlurKernel { taps })
        })
    });
    let _pending = declared.declare(|| {
        let pending_builds = Arc::clone(&pending_builds);
        computed(move || {
            pending_builds.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(BlurKernel { taps: *taps.get() })
        })
    });
    let (scope, view) = declared.start().unwrap();
    // Bound after the scope, so it drops first and releases the build if the test unwinds.
    let release_first_build = release_first_build;
    started.recv_timeout(SETTLE).unwrap();

    let (closed, closed_now) = mpsc::channel();
    let closing = std::thread::spawn(move || {
        drop(scope);
        closed.send(()).unwrap();
    });
    // The close marks the scope closing before it waits for the build in flight.
    std::thread::sleep(Duration::from_millis(100));
    drop(release_first_build);
    closed_now.recv_timeout(SETTLE).unwrap();
    closing.join().unwrap();
    drop(view);
    assert_eq!(pending_builds.load(Ordering::SeqCst), 0);
}

/// A build error of the build's own type: the scope needs only its `Display`.
#[derive(Debug)]
struct EvenTapCount(u32);

impl fmt::Display for EvenTapCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a blur needs an odd tap count, not {}", self.0)
    }
}

#[test]
fn a_failed_build_keeps_the_previous_value_and_is_not_retried_until_its_input_changes() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let kernel = declared.declare(|| {
        let builds = Arc::clone(&builds);
        computed(move || {
            builds.fetch_add(1, Ordering::SeqCst);
            let taps = *taps.get();
            if taps.is_multiple_of(2) {
                return Err(EvenTapCount(taps));
            }
            Ok(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || true);

    scope.write(|writes| writes.write(taps, 4));
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(view.run_frame(|| kernel.get().taps), Some(3));
    assert!(matches!(
        scope.last_build_of(kernel),
        LastBuild::Failed { why: BuildFailure::Returned(why), .. }
            if why == "a blur needs an odd tap count, not 4"
    ));
    let after_failure = builds.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(builds.load(Ordering::SeqCst), after_failure);

    scope.write(|writes| writes.write(taps, 7));
    run_frames_until(&mut view, || kernel.get().taps == 7);
    assert!(matches!(
        scope.last_build_of(kernel),
        LastBuild::Built { .. }
    ));
}

#[test]
fn a_build_reading_a_computed_value_not_yet_built_fails_and_builds_again_once_it_is_built() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(4_u32);
    let (odd_taps, kernel) = declared.declare(|| {
        let odd_taps = computed(move || {
            let taps = *taps.get();
            if taps.is_multiple_of(2) {
                return Err(EvenTapCount(taps));
            }
            Ok(taps)
        });
        let kernel = computed(move || {
            Ok::<_, String>(BlurKernel {
                taps: *odd_taps.get(),
            })
        });
        (odd_taps, kernel)
    });
    let (scope, mut view) = declared.start().unwrap();
    assert!(scope.wait_until_settled(SETTLE));
    assert!(matches!(
        scope.last_build_of(odd_taps),
        LastBuild::Failed {
            why: BuildFailure::Returned(_),
            ..
        }
    ));
    assert_eq!(
        scope.last_build_of(kernel),
        LastBuild::Failed {
            builds: 1,
            why: BuildFailure::ReadAComputedValueNotYetBuilt
        }
    );
    assert_eq!(view.run_frame(|| ()), None);

    scope.write(|writes| writes.write(taps, 5));
    run_frames_until(&mut view, || kernel.get().taps == 5);
    assert_eq!(scope.last_build_of(kernel), LastBuild::Built { builds: 2 });
}

#[test]
fn a_build_that_panics_fails_with_its_message_and_the_worker_keeps_building() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(0_u32);
    let kernel = declared.declare(|| {
        computed(move || {
            let taps = *taps.get();
            assert!(taps != 0, "no taps");
            Ok::<_, String>(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(
        scope.last_build_of(kernel),
        LastBuild::Failed {
            builds: 1,
            why: BuildFailure::Panicked("no taps".to_string())
        }
    );

    scope.write(|writes| writes.write(taps, 3));
    run_frames_until(&mut view, || kernel.get().taps == 3);
}

#[test]
#[should_panic(expected = "`.get()` reads only in a build or a frame")]
fn get_outside_a_build_or_a_frame_is_refused() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let _ = taps.get();
}

#[test]
#[should_panic(expected = "`.get()` reads only in a build or a frame")]
fn get_in_the_body_is_refused() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    declared.declare(|| {
        let _ = taps.get();
    });
}

#[test]
#[should_panic(expected = "`computed` declares only while a scope is being declared")]
fn computed_outside_a_declaration_is_refused() {
    let _ = computed(|| Ok::<_, String>(1_u32));
}

#[test]
#[should_panic(expected = "the handle belongs to another scope")]
fn a_frame_reading_another_scopes_input_is_refused() {
    let mut other = ScopeBeingDeclared::new();
    let others_taps = other.input(3_u32);
    let (_scope, mut view) = ScopeBeingDeclared::new().start().unwrap();
    view.run_frame(|| others_taps.get());
}

/// A value that records the thread it was dropped on.
struct DroppedOn {
    value: u32,
    drops: Arc<Mutex<Vec<ThreadId>>>,
}

impl DroppedOn {
    fn new(value: u32, drops: &Arc<Mutex<Vec<ThreadId>>>) -> Self {
        Self {
            value,
            drops: Arc::clone(drops),
        }
    }
}

impl PartialEq for DroppedOn {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl Drop for DroppedOn {
    fn drop(&mut self) {
        self.drops.lock().unwrap().push(std::thread::current().id());
    }
}

/// Runs frames on a thread of their own until `done`, and returns that thread's id.
fn frame_thread_runs_until(view: &mut FrameView, done: impl FnMut() -> bool + Send) -> ThreadId {
    std::thread::scope(|threads| {
        threads
            .spawn(move || {
                run_frames_until(view, done);
                std::thread::current().id()
            })
            .join()
            .unwrap()
    })
}

#[test]
fn a_value_the_frame_replaces_is_never_dropped_on_the_frame_thread() {
    let inputs_dropped_on = Arc::new(Mutex::new(Vec::new()));
    let builds_dropped_on = Arc::new(Mutex::new(Vec::new()));
    let mut declared = ScopeBeingDeclared::new();
    let level = declared.input(DroppedOn::new(0, &inputs_dropped_on));
    let built = declared.declare(|| {
        let builds_dropped_on = Arc::clone(&builds_dropped_on);
        computed(move || Ok::<_, String>(DroppedOn::new(level.get().value, &builds_dropped_on)))
    });
    let (scope, mut view) = declared.start().unwrap();
    let mut frame_threads = Vec::new();
    for value in 1..=20 {
        scope.write(|writes| writes.write(level, DroppedOn::new(value, &inputs_dropped_on)));
        frame_threads.push(frame_thread_runs_until(&mut view, || {
            level.get().value == value && built.get().value == value
        }));
    }
    assert!(scope.wait_until_settled(SETTLE));
    drop(scope);
    let inputs_dropped_on = inputs_dropped_on.lock().unwrap();
    let builds_dropped_on = builds_dropped_on.lock().unwrap();
    assert!(!inputs_dropped_on.is_empty() && !builds_dropped_on.is_empty());
    for dropped_on in inputs_dropped_on.iter().chain(builds_dropped_on.iter()) {
        assert!(
            !frame_threads.contains(dropped_on),
            "a value was dropped on a frame thread"
        );
    }
}

#[test]
fn a_value_the_frame_replaces_in_a_scope_of_inputs_alone_is_dropped_by_the_writer() {
    let dropped_on = Arc::new(Mutex::new(Vec::new()));
    let mut declared = ScopeBeingDeclared::new();
    let level = declared.input(DroppedOn::new(0, &dropped_on));
    let (scope, mut view) = declared.start().unwrap();
    let mut frame_threads = Vec::new();
    for value in 1..=20 {
        scope.write(|writes| writes.write(level, DroppedOn::new(value, &dropped_on)));
        frame_threads.push(frame_thread_runs_until(&mut view, || {
            level.get().value == value
        }));
    }
    // Closing drops the one value the frame replaced since the last write.
    drop(scope);
    let dropped_on = dropped_on.lock().unwrap();
    assert_eq!(dropped_on.len(), 20);
    for dropped_on in dropped_on.iter() {
        assert!(
            !frame_threads.contains(dropped_on),
            "a value was dropped on a frame thread"
        );
    }
}

/// An input whose comparison panics once its value is 1.
struct PanicsOnCompare(u32);

impl PartialEq for PanicsOnCompare {
    fn eq(&self, other: &Self) -> bool {
        assert!(self.0 != 1 && other.0 != 1, "compared a poisoned value");
        self.0 == other.0
    }
}

#[test]
fn a_comparison_that_panics_in_a_write_fails_the_write_and_the_scope_still_closes() {
    let mut declared = ScopeBeingDeclared::new();
    let level = declared.input(PanicsOnCompare(0));
    let (scope, view) = declared.start().unwrap();
    // The scope is closed as the panic unwinds, as a node's is when its writer fails.
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        scope.write(|writes| writes.write(level, PanicsOnCompare(1)));
        drop(scope);
    }));
    assert!(failed.is_err());
    drop(view);
}

#[test]
fn a_comparison_that_panics_in_a_write_refuses_the_whole_write_and_the_scope_keeps_working() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let level = declared.input(PanicsOnCompare(0));
    let (scope, mut view) = declared.start().unwrap();
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scope.write(|writes| {
            writes.write(taps, 9);
            writes.write(level, PanicsOnCompare(1));
        });
    }));
    assert!(failed.is_err());
    assert_eq!(
        view.run_frame(|| (*taps.get(), level.get().0)),
        Some((3, 0))
    );

    scope.write(|writes| {
        writes.write(taps, 9);
        writes.write(level, PanicsOnCompare(2));
    });
    assert_eq!(
        view.run_frame(|| (*taps.get(), level.get().0)),
        Some((9, 2))
    );
}

#[test]
fn a_value_the_frame_replaced_is_dropped_by_a_write_of_any_input() {
    let alive = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let buffer = declared.input(Counted::new(0, &alive));
    let level = declared.input(0_u32);
    let (scope, mut view) = declared.start().unwrap();

    scope.write(|writes| writes.write(buffer, Counted::new(1, &alive)));
    assert_eq!(view.run_frame(|| buffer.get().value), Some(1));
    // The frame replaced the first buffer; a write of another input drops it.
    scope.write(|writes| writes.write(level, 1));
    assert_eq!(alive.load(Ordering::SeqCst), 1);
}

/// A built value whose comparison panics, as a value's `PartialEq` might on some state.
#[derive(Debug)]
struct PanicsWhenCompared(u32);

impl PartialEq for PanicsWhenCompared {
    fn eq(&self, _other: &Self) -> bool {
        panic!("compared a built value")
    }
}

#[test]
fn a_built_value_whose_comparison_panics_fails_its_build_and_the_scope_keeps_working() {
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let kernel =
        declared.declare(|| computed(move || Ok::<_, String>(PanicsWhenCompared(*taps.get()))));
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || kernel.get().0 == 3);

    scope.write(|writes| writes.write(taps, 4));
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(
        scope.last_build_of(kernel),
        LastBuild::Failed {
            builds: 2,
            why: BuildFailure::ComparisonPanicked("compared a built value".to_string())
        }
    );
    // The previous value keeps serving, and the scope still takes writes.
    run_frames_until(&mut view, || *taps.get() == 4);
    assert_eq!(view.run_frame(|| kernel.get().0), Some(3));
    scope.write(|writes| writes.write(taps, 5));
    assert!(scope.wait_until_settled(SETTLE));
}
