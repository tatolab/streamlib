// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use StubExclusiveDeviceEvent::{Closed, Opened, Released};

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

/// Mental-revert: an open beside the holder leaves two holders, the first closed only after the
/// second opened.
#[test]
fn an_open_while_it_is_held_closes_the_holder_first_and_its_drop_only_releases_it() {
    let device = Arc::new(StubExclusiveDevice::default());
    let first = device.open();

    let second = device.open();
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
    drop(second);
    assert_eq!(device.closes(), 2);
}
