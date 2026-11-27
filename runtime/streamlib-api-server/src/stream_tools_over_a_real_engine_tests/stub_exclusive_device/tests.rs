// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use StubExclusiveDeviceEvent::{Closed, Opened, Released, TakenOver};

#[test]
fn one_holder_after_another_opens_and_closes_it_each_time() {
    let device = Arc::new(StubExclusiveDevice::default());

    drop(device.open());
    let second = device.open();

    assert_eq!((device.opens(), device.closes()), (2, 1));
    drop(second);
    assert_eq!(
        device.events(),
        [
            Opened { holder: 0 },
            Closed { holder: 0 },
            Opened { holder: 1 },
            Closed { holder: 1 }
        ]
    );
}

/// Mental-revert: an open beside the holder leaves two holders; one recorded as a close hides
/// that the holder never closed.
#[test]
fn an_open_while_it_is_held_takes_it_over_and_the_holders_drop_only_releases_it() {
    let device = Arc::new(StubExclusiveDevice::default());
    let first = device.open();

    let second = device.open();
    drop(first);

    assert_eq!(
        device.events(),
        [
            Opened { holder: 0 },
            TakenOver { holder: 0 },
            Opened { holder: 1 },
            Released { holder: 0 }
        ]
    );
    drop(second);
    assert_eq!(device.closes(), 1);
}

/// Mental-revert: a close that left the lease holding the device would be followed by a takeover.
#[test]
fn a_holder_closed_before_the_next_open_is_closed_and_its_drop_only_releases_it() {
    let device = Arc::new(StubExclusiveDevice::default());
    let first = device.open();

    first.close();
    let _second = device.open();
    drop(first);

    assert_eq!(
        device.events(),
        [
            Opened { holder: 0 },
            Closed { holder: 0 },
            Opened { holder: 1 },
            Released { holder: 0 }
        ]
    );
}
