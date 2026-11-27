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

/// A camera, and the capture holding it while that capture is held at all.
type CameraAndItsCapture<C> = (String, Weak<Mutex<Option<C>>>);

/// The cameras captures of type `C` hold in this process, each by one capture.
pub struct CamerasHeldByOneCaptureEach<C> {
    /// Held across a takeover, so two opens of one camera never both find it free.
    opening: Mutex<()>,
    held: Mutex<Vec<CameraAndItsCapture<C>>>,
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
mod tests;
