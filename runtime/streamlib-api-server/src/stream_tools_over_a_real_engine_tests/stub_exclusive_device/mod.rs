// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A device only one holder may have open at once, as a camera or a microphone is, standing in
//! for one in a test. An open while another holder has it takes it over, as a real device's
//! layer does: the earlier holder is closed first, so two holders never have it. It records each
//! open and close in order, and each lease dropped after its device was taken from it.

use std::sync::Arc;

use parking_lot::Mutex;

/// One open or close of a [`StubExclusiveDevice`], or a lease dropped once a later open took the
/// device from it, naming the holder by the count of opens before its own: the first holder is 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StubExclusiveDeviceEvent {
    Opened { holder: usize },
    Closed { holder: usize },
    Released { holder: usize },
}

/// What a [`StubExclusiveDevice`] has seen since it was made.
#[derive(Debug, Default)]
struct StubExclusiveDeviceOpensAndCloses {
    held_by: Option<usize>,
    events: Vec<StubExclusiveDeviceEvent>,
}

impl StubExclusiveDeviceOpensAndCloses {
    fn count(&self, counted: fn(&StubExclusiveDeviceEvent) -> bool) -> usize {
        self.events.iter().filter(|event| counted(event)).count()
    }
}

/// A device one holder at a time may have open; an open while it is held closes the holder
/// that has it first.
#[derive(Debug, Default)]
pub(super) struct StubExclusiveDevice {
    held_so_far: Mutex<StubExclusiveDeviceOpensAndCloses>,
}

/// One holder's hold on a [`StubExclusiveDevice`], closed when dropped unless a later open took
/// the device over first.
#[derive(Debug)]
pub(super) struct StubExclusiveDeviceLease {
    device: Arc<StubExclusiveDevice>,
    holder: usize,
}

impl PartialEq for StubExclusiveDeviceLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.device, &other.device) && self.holder == other.holder
    }
}

impl StubExclusiveDevice {
    /// Open the device for a new holder, closing the holder that has it first.
    pub(super) fn open(self: &Arc<Self>) -> StubExclusiveDeviceLease {
        let mut held_so_far = self.held_so_far.lock();
        if let Some(taken_from) = held_so_far.held_by.take() {
            held_so_far
                .events
                .push(StubExclusiveDeviceEvent::Closed { holder: taken_from });
        }
        let holder =
            held_so_far.count(|event| matches!(event, StubExclusiveDeviceEvent::Opened { .. }));
        held_so_far.held_by = Some(holder);
        held_so_far
            .events
            .push(StubExclusiveDeviceEvent::Opened { holder });
        StubExclusiveDeviceLease {
            device: Arc::clone(self),
            holder,
        }
    }

    /// Every open and close of the device, in the order they happened.
    pub(super) fn events(&self) -> Vec<StubExclusiveDeviceEvent> {
        self.held_so_far.lock().events.clone()
    }

    /// How many times the device was opened.
    pub(super) fn opens(&self) -> usize {
        self.held_so_far
            .lock()
            .count(|event| matches!(event, StubExclusiveDeviceEvent::Opened { .. }))
    }

    /// How many leases were dropped after a later open took the device from them.
    pub(super) fn releases(&self) -> usize {
        self.held_so_far
            .lock()
            .count(|event| matches!(event, StubExclusiveDeviceEvent::Released { .. }))
    }

    /// How many times the device was closed.
    pub(super) fn closes(&self) -> usize {
        self.held_so_far
            .lock()
            .count(|event| matches!(event, StubExclusiveDeviceEvent::Closed { .. }))
    }
}

impl Drop for StubExclusiveDeviceLease {
    fn drop(&mut self) {
        let mut held_so_far = self.device.held_so_far.lock();
        let holder = self.holder;
        let dropped = if held_so_far.held_by == Some(holder) {
            held_so_far.held_by = None;
            StubExclusiveDeviceEvent::Closed { holder }
        } else {
            StubExclusiveDeviceEvent::Released { holder }
        };
        held_so_far.events.push(dropped);
    }
}

#[cfg(test)]
mod tests;
