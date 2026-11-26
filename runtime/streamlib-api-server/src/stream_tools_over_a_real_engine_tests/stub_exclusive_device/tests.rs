// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

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
