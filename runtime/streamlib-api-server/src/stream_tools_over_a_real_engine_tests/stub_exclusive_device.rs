// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A device only one holder may have open at once, as a camera or a microphone is, standing in
//! for one in a test: it counts each open and close, and any read of it fails the test once a
//! second holder ever asked for it while it was held.

use std::sync::Arc;

use parking_lot::Mutex;

/// What a [`StubExclusiveDevice`] has seen since it was made.
#[derive(Debug, Default)]
struct HeldSoFar {
    holder: bool,
    opens: usize,
    closes: usize,
    opens_refused_while_held: usize,
}

/// A device one holder at a time may open; a second open while it is held is refused, as a
/// busy device refuses it, and recorded against the test.
#[derive(Debug, Default)]
pub(super) struct StubExclusiveDevice {
    held_so_far: Mutex<HeldSoFar>,
}

/// The one open hold on a [`StubExclusiveDevice`], which closes it when dropped.
#[derive(Debug)]
pub(super) struct StubExclusiveDeviceLease {
    device: Arc<StubExclusiveDevice>,
}

/// A second holder asked for a [`StubExclusiveDevice`] while another held it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct StubExclusiveDeviceAlreadyHeld;

impl std::fmt::Display for StubExclusiveDeviceAlreadyHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the stub exclusive device is already held by another holder")
    }
}

impl StubExclusiveDevice {
    /// Open the device for one holder, refused while another holds it.
    pub(super) fn open(
        self: &Arc<Self>,
    ) -> std::result::Result<StubExclusiveDeviceLease, StubExclusiveDeviceAlreadyHeld> {
        let mut held_so_far = self.held_so_far.lock();
        if held_so_far.holder {
            held_so_far.opens_refused_while_held += 1;
            return Err(StubExclusiveDeviceAlreadyHeld);
        }
        held_so_far.holder = true;
        held_so_far.opens += 1;
        Ok(StubExclusiveDeviceLease {
            device: Arc::clone(self),
        })
    }

    /// How many times the device was opened; fails the test if two holders ever asked for it
    /// at once.
    pub(super) fn opens(&self) -> usize {
        self.held_so_far_by_one_holder_at_a_time().opens
    }

    /// How many times the device was closed; fails the test if two holders ever asked for it
    /// at once.
    pub(super) fn closes(&self) -> usize {
        self.held_so_far_by_one_holder_at_a_time().closes
    }

    fn held_so_far_by_one_holder_at_a_time(&self) -> parking_lot::MutexGuard<'_, HeldSoFar> {
        let held_so_far = self.held_so_far.lock();
        assert_eq!(
            held_so_far.opens_refused_while_held, 0,
            "two holders asked for the stub exclusive device at once: {held_so_far:?}"
        );
        held_so_far
    }
}

impl Drop for StubExclusiveDeviceLease {
    fn drop(&mut self) {
        let mut held_so_far = self.device.held_so_far.lock();
        held_so_far.holder = false;
        held_so_far.closes += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_after_another_opens_and_closes_it_each_time() {
        let device = Arc::new(StubExclusiveDevice::default());

        drop(device.open().expect("the first holder opens it"));
        let second = device
            .open()
            .expect("the next holder opens it once it is closed");

        assert_eq!((device.opens(), device.closes()), (2, 1));
        drop(second);
        assert_eq!(device.closes(), 2);
    }

    #[test]
    #[should_panic(expected = "two holders asked for the stub exclusive device at once")]
    fn a_second_holder_while_it_is_held_is_refused_and_fails_the_test() {
        let device = Arc::new(StubExclusiveDevice::default());
        let _first = device.open().expect("the first holder opens it");

        assert_eq!(device.open().unwrap_err(), StubExclusiveDeviceAlreadyHeld);

        device.opens();
    }
}
