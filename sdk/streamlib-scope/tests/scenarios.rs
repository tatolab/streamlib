// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Blur taps, a compositor gaining a layer and an exclusive device held as a computed value, run
//! against the scope with a frame loop standing in for a node's per-frame function.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

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
    scope.write(taps, 9);
    let while_building = view.run_frame(|| (*taps.get(), kernel.get().taps));
    builds_may_finish.store(true, Ordering::SeqCst);
    assert_eq!(while_building, Some((9, 5)));
    run_frames_until(&mut view, || kernel.get().taps == 9);

    // The kernel never read the radius, so a radius drag builds nothing.
    let before = builds.load(Ordering::SeqCst);
    for step in 0..50 {
        scope.write(radius, step as f32);
    }
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(builds.load(Ordering::SeqCst), before);
}

#[test]
fn a_burst_of_tap_counts_ends_on_the_newest_and_throws_stale_builds_away() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(1_u32);
    let kernel = declared.declare(|| {
        let builds = Arc::clone(&builds);
        computed(move || {
            let taps = *taps.get();
            builds.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            Ok::<_, String>(BlurKernel { taps })
        })
    });
    let (scope, mut view) = declared.start().unwrap();
    run_frames_until(&mut view, || true);

    let mut handed_over = Vec::new();
    for taps_now in 2..=30 {
        scope.write(taps, taps_now);
        std::thread::sleep(Duration::from_millis(2));
        handed_over.extend(view.run_frame(|| kernel.get().taps));
    }
    run_frames_until(&mut view, || kernel.get().taps == 30);

    // 29 writes, fewer builds, and no stale kernel ever handed over after a newer one.
    assert!(
        builds.load(Ordering::SeqCst) < 29,
        "builds: {}",
        builds.load(Ordering::SeqCst)
    );
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
    scope.write(taps, 2);
    drop(release_tx);

    run_frames_until(&mut view, || kernel.get().taps == 2);
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(scope.last_build_of(kernel), LastBuild::Built { builds: 1 });
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
        scope.write(taps, 3);
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
        scope.write(
            layers,
            vec![
                Layer {
                    opacity: step as f32 / 40.0,
                },
                Layer { opacity: 1.0 },
            ],
        );
    }
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(kernel_builds.load(Ordering::SeqCst), after_first);

    // Wire the empty slot: a third layer. The kernels follow without a rebuild of the node.
    scope.write(
        layers,
        vec![
            Layer { opacity: 1.0 },
            Layer { opacity: 1.0 },
            Layer { opacity: 0.5 },
        ],
    );
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

    scope.write(device_id, 1);
    run_frames_until(&mut view, || camera.get().frame() == Some(1));
    // The replaced lease is released off the frame thread; one holder throughout.
    assert!(scope.wait_until_settled(SETTLE));
    assert_eq!(device.open_holders.load(Ordering::SeqCst), 1);
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

    scope.write(taps, 4);
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

    scope.write(taps, 7);
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

    scope.write(taps, 5);
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

    scope.write(taps, 3);
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
