// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_stream_given_up_stops_once_and_runs_nothing_after() {
    let on_a_device = AudioStreamOnADevice::opened_on(Some("hw:1".to_owned()), Box::new(0_u32));

    on_a_device.give_its_device_up(|stops| *stops += 1);
    on_a_device.give_its_device_up(|_| panic!("a stream given up stops once"));

    assert_eq!(on_a_device.while_it_holds_its_device(|_| ()), None);
}

#[test]
fn a_stream_is_on_the_device_it_was_opened_on_and_on_no_other() {
    let on_the_default = AudioStreamOnADevice::opened_on(None, Box::new(()));
    let on_a_named_one = AudioStreamOnADevice::opened_on(Some("hw:1".to_owned()), Box::new(()));

    assert!(on_the_default.is_on(None));
    assert!(!on_the_default.is_on(Some("hw:1")));
    assert!(on_a_named_one.is_on(Some("hw:1")));
    assert!(!on_a_named_one.is_on(None));
}
