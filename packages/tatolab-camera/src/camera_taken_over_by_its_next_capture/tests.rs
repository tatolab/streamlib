// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A capture standing in for a camera's, recording its drop.
struct CaptureOfATest {
    named: &'static str,
    closed: Arc<Mutex<Vec<&'static str>>>,
}

impl Drop for CaptureOfATest {
    fn drop(&mut self) {
        locked(&self.closed).push(self.named);
    }
}

fn opened(
    cameras: &CamerasHeldByOneCaptureEach<CaptureOfATest>,
    camera: &str,
    named: &'static str,
    closed: &Arc<Mutex<Vec<&'static str>>>,
) -> CaptureHoldingACamera<CaptureOfATest> {
    cameras
        .open_taking_over(camera, || {
            Ok::<_, ()>(CaptureOfATest {
                named,
                closed: Arc::clone(closed),
            })
        })
        .unwrap()
}

/// Mental-revert: a capture opened beside the one holding its camera leaves two holders.
#[test]
fn a_capture_of_a_held_camera_closes_the_holder_before_it_opens() {
    let cameras = CamerasHeldByOneCaptureEach::new();
    let closed = Arc::new(Mutex::new(Vec::new()));
    let first = opened(&cameras, "/dev/video0", "first", &closed);

    let second = cameras
        .open_taking_over("/dev/video0", || {
            assert_eq!(
                *locked(&closed),
                ["first"],
                "the holder closed before this open"
            );
            Ok::<_, ()>(CaptureOfATest {
                named: "second",
                closed: Arc::clone(&closed),
            })
        })
        .unwrap();

    assert_eq!(first.while_it_holds_its_camera(|_| ()), None);
    first.close();
    assert_eq!(
        *locked(&closed),
        ["first"],
        "a capture taken over closes once"
    );
    assert_eq!(
        second.while_it_holds_its_camera(|capture| capture.named),
        Some("second")
    );
}

#[test]
fn a_capture_of_another_camera_leaves_the_first_capturing() {
    let cameras = CamerasHeldByOneCaptureEach::new();
    let closed = Arc::new(Mutex::new(Vec::new()));
    let first = opened(&cameras, "/dev/video0", "first", &closed);

    let _second = opened(&cameras, "/dev/video2", "second", &closed);

    assert!(locked(&closed).is_empty());
    assert_eq!(
        first.while_it_holds_its_camera(|capture| capture.named),
        Some("first")
    );
}

#[test]
fn a_capture_closed_or_dropped_gives_its_camera_back() {
    let cameras = CamerasHeldByOneCaptureEach::new();
    let closed = Arc::new(Mutex::new(Vec::new()));
    opened(&cameras, "/dev/video0", "first", &closed).close();
    drop(opened(&cameras, "/dev/video0", "second", &closed));

    let _third = opened(&cameras, "/dev/video0", "third", &closed);

    assert_eq!(*locked(&closed), ["first", "second"]);
}
