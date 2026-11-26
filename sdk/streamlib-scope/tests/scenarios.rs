// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The spike's two scenarios, blur taps and a compositor gaining a layer, plus an exclusive
//! device held as a derived value, run against the scope with a frame loop standing in for
//! `process`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use streamlib_scope::{LastBuild, ScopeBeingDeclared};

/// A kernel built for a tap count, standing in for a compiled blur shader.
#[derive(Debug, PartialEq)]
struct BlurKernel {
    taps: u32,
}

fn run_frames_until(
    view: &mut streamlib_scope::FrameView,
    mut done: impl FnMut(&streamlib_scope::FrameView) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        view.flush();
        if done(view) {
            return;
        }
        assert!(Instant::now() < deadline, "the frame loop waited too long");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn blur_keeps_its_old_kernel_until_the_new_one_is_built_and_ignores_the_radius() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(5_u32);
    let radius = declared.input(1.0_f32);
    let kernel = {
        let builds = Arc::clone(&builds);
        declared.derived(move |reads| {
            let taps = *reads.input(taps);
            builds.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(30));
            Ok(BlurKernel { taps })
        })
    };
    let (scope, mut view) = declared.start();

    // Not ready until the first build is handed over.
    view.flush();
    assert!(view.derived(kernel).is_none());
    run_frames_until(&mut view, |view| view.derived(kernel).is_some());
    assert_eq!(view.derived(kernel), Some(&BlurKernel { taps: 5 }));

    // A new tap count: frames keep the 5-tap kernel while the 9-tap one builds.
    scope.write(taps, 9);
    view.flush();
    assert_eq!(*view.input(taps), 9);
    assert_eq!(view.derived(kernel), Some(&BlurKernel { taps: 5 }));
    run_frames_until(&mut view, |view| {
        view.derived(kernel) == Some(&BlurKernel { taps: 9 })
    });

    // The kernel never read the radius, so a radius drag builds nothing.
    let before = builds.load(Ordering::SeqCst);
    for step in 0..50 {
        scope.write(radius, step as f32);
    }
    scope.wait_until_settled();
    assert_eq!(builds.load(Ordering::SeqCst), before);
}

#[test]
fn a_burst_of_tap_counts_ends_on_the_newest_and_throws_stale_builds_away() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(1_u32);
    let kernel = {
        let builds = Arc::clone(&builds);
        declared.derived(move |reads| {
            let taps = *reads.input(taps);
            builds.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            Ok(BlurKernel { taps })
        })
    };
    let (scope, mut view) = declared.start();
    run_frames_until(&mut view, |view| view.derived(kernel).is_some());

    let handed_over = Arc::new(Mutex::new(Vec::new()));
    for taps_now in 2..=30 {
        scope.write(taps, taps_now);
        std::thread::sleep(Duration::from_millis(2));
        view.flush();
        if let Some(seen) = view.derived(kernel) {
            handed_over.lock().unwrap().push(seen.taps);
        }
    }
    run_frames_until(&mut view, |view| {
        view.derived(kernel) == Some(&BlurKernel { taps: 30 })
    });

    // 29 writes, far fewer builds, and no stale kernel ever handed over after a newer one.
    assert!(
        builds.load(Ordering::SeqCst) < 15,
        "builds: {}",
        builds.load(Ordering::SeqCst)
    );
    let seen = handed_over.lock().unwrap();
    assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
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
    // The slot list, derived by the engine from the node's links.
    let layers = declared.input(vec![Layer { opacity: 1.0 }, Layer { opacity: 1.0 }]);
    let layer_count = declared.derived(move |reads| Ok(reads.input(layers).len()));
    let kernels = {
        let kernel_builds = Arc::clone(&kernel_builds);
        declared.derived(move |reads| {
            let count = *reads
                .derived(layer_count)
                .ok_or("the layer count is not built yet")?;
            kernel_builds.fetch_add(1, Ordering::SeqCst);
            Ok((0..count)
                .map(|_| BlurKernel { taps: 1 })
                .collect::<Vec<_>>())
        })
    };
    let (scope, mut view) = declared.start();
    run_frames_until(&mut view, |view| {
        view.derived(kernels).is_some_and(|k| k.len() == 2)
    });
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
    scope.wait_until_settled();
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
    run_frames_until(&mut view, |view| {
        view.derived(kernels).is_some_and(|k| k.len() == 3)
    });
    assert_eq!(view.input(layers).len(), 3);
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
    let camera = {
        let device = Arc::clone(&device);
        declared.derived(move |reads| Ok(device.open(*reads.input(device_id))))
    };
    let (scope, mut view) = declared.start();
    run_frames_until(&mut view, |view| {
        view.derived(camera).and_then(DeviceLease::frame) == Some(0)
    });

    scope.write(device_id, 1);
    run_frames_until(&mut view, |view| {
        view.derived(camera).and_then(DeviceLease::frame) == Some(1)
    });
    // The replaced lease is released off the frame thread; one holder throughout.
    scope.wait_until_settled();
    assert_eq!(device.open_holders.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failed_build_keeps_the_previous_value_and_is_not_retried_until_its_input_changes() {
    let builds = Arc::new(AtomicUsize::new(0));
    let mut declared = ScopeBeingDeclared::new();
    let taps = declared.input(3_u32);
    let kernel = {
        let builds = Arc::clone(&builds);
        declared.derived(move |reads| {
            builds.fetch_add(1, Ordering::SeqCst);
            let taps = *reads.input(taps);
            if taps % 2 == 0 {
                return Err(format!("a blur needs an odd tap count, not {taps}"));
            }
            Ok(BlurKernel { taps })
        })
    };
    let (scope, mut view) = declared.start();
    run_frames_until(&mut view, |view| view.derived(kernel).is_some());

    scope.write(taps, 4);
    scope.wait_until_settled();
    view.flush();
    assert_eq!(view.derived(kernel), Some(&BlurKernel { taps: 3 }));
    assert!(matches!(
        scope.last_build_of(kernel),
        LastBuild::Failed { .. }
    ));
    let after_failure = builds.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(builds.load(Ordering::SeqCst), after_failure);

    scope.write(taps, 7);
    run_frames_until(&mut view, |view| {
        view.derived(kernel) == Some(&BlurKernel { taps: 7 })
    });
    assert!(matches!(
        scope.last_build_of(kernel),
        LastBuild::Built { .. }
    ));
}
