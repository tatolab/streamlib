// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The camera each capture in this process holds. A capture opening a camera another capture
//! holds closes that capture first, so the capture a camera node opens in place of another on the
//! same camera takes the camera over, and two captures never hold one camera.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The capture a camera node holds, which closes when dropped or closed, or when a later capture
/// takes its camera over.
type CaptureUntilClosed<C> = Arc<Mutex<Option<C>>>;

/// The cameras captures of type `C` hold in this process, each by one capture.
pub struct CamerasHeldByOneCaptureEach<C> {
    /// Held across a takeover, so two opens of one camera never both find it free.
    opening: Mutex<()>,
    held: Mutex<Vec<(String, Weak<Mutex<Option<C>>>)>>,
}

/// One capture holding its camera, until it is closed or a later capture takes the camera over.
pub struct CaptureHoldingACamera<C> {
    capture: CaptureUntilClosed<C>,
}

impl<C> CamerasHeldByOneCaptureEach<C> {
    pub const fn new() -> Self {
        Self {
            opening: Mutex::new(()),
            held: Mutex::new(Vec::new()),
        }
    }

    /// Close the capture holding `camera`, if one does, then open a capture of it with `open`.
    /// A failed open leaves the camera held by nothing.
    pub fn open_taking_over<E>(
        &self,
        camera: &str,
        open: impl FnOnce() -> Result<C, E>,
    ) -> Result<CaptureHoldingACamera<C>, E> {
        let _opening = locked(&self.opening);
        let mut taken_over = Vec::new();
        locked(&self.held).retain(|(held_camera, capture)| match capture.upgrade() {
            Some(capture) if held_camera == camera => {
                taken_over.push(capture);
                false
            }
            Some(_) => true,
            None => false,
        });
        for capture in taken_over {
            close(&capture);
        }
        let capture = Arc::new(Mutex::new(Some(open()?)));
        locked(&self.held).push((camera.to_owned(), Arc::downgrade(&capture)));
        Ok(CaptureHoldingACamera { capture })
    }
}

impl<C> Default for CamerasHeldByOneCaptureEach<C> {
    fn default() -> Self {
        Self::new()
    }
}

fn close<C>(capture: &Mutex<Option<C>>) {
    // Taken out before it drops, so its drop, which stops its thread, runs with no lock held.
    let closed = locked(capture).take();
    drop(closed);
}

impl<C> CaptureHoldingACamera<C> {
    /// Run `with` on the capture, or answer `None` once it is closed or its camera was taken over.
    pub fn while_it_holds_its_camera<T>(&self, with: impl FnOnce(&mut C) -> T) -> Option<T> {
        locked(&self.capture).as_mut().map(with)
    }

    /// Close the capture, giving its camera back; a capture already closed, or whose camera a
    /// later capture took over, stays as it is.
    pub fn close(&self) {
        close(&self.capture);
    }
}

#[cfg(test)]
mod tests {
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
}
