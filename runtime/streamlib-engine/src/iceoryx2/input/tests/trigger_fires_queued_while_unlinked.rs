// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::iceoryx2::DeliveryProfile;

#[test]
fn each_fire_is_read_once_in_order_as_an_empty_records_message_stamped_when_fired() {
    let mailboxes = InputMailboxesInner::new();
    assert!(!mailboxes.has_data("go"));
    assert!(!mailboxes.any_port_has_data());

    for fired_at_ns in [10, 20, 30] {
        mailboxes.queue_a_trigger_fire("go", fired_at_ns).unwrap();
    }
    assert!(mailboxes.has_data("go"));
    assert!(!mailboxes.has_data("other"));
    assert!(
        mailboxes.any_port_has_data(),
        "a waiting fire wakes the node"
    );

    let mut read = Vec::new();
    while let Some(fire) = mailboxes.read_raw("go").unwrap() {
        assert_eq!(&*fire.payload, &[0x80], "the empty map in msgpack");
        assert_eq!(
            fire.payload_descriptor,
            DataChannelPayloadDescriptor::records_message()
        );
        assert_eq!(fire.group_boundary, MessageGroupBoundary::StartsGroup);
        assert_eq!(fire.lost_since_last_read, 0);
        read.push(fire.timestamp_ns);
    }
    assert_eq!(read, [10, 20, 30]);
    assert!(!mailboxes.has_data("go"));
    assert!(!mailboxes.any_port_has_data());
}

#[test]
fn a_fire_past_the_ordered_depth_drops_the_oldest_and_the_next_read_is_told() {
    let mailboxes = InputMailboxesInner::new();
    let fires = DeliveryProfile::ORDERED_DEPTH as i64 + 3;
    for fired_at_ns in 0..fires {
        mailboxes.queue_a_trigger_fire("go", fired_at_ns).unwrap();
    }

    let first = mailboxes.read_raw("go").unwrap().expect("a fire waits");
    assert_eq!(first.timestamp_ns, 3, "the three oldest were dropped");
    assert_eq!(first.lost_since_last_read, 3);
    let second = mailboxes.read_raw("go").unwrap().expect("a fire waits");
    assert_eq!(second.lost_since_last_read, 0, "each loss is told once");
    let mut remaining = 0;
    while mailboxes.read_raw("go").unwrap().is_some() {
        remaining += 1;
    }
    assert_eq!(remaining, DeliveryProfile::ORDERED_DEPTH - 2);
}

#[test]
fn a_queued_fire_makes_the_reactive_loop_wake_readable_until_it_is_drained() {
    let mailboxes = InputMailboxesInner::new();
    let wake_fd = mailboxes
        .reactive_loop_wake_fd()
        .expect("the mailboxes own a wake");
    let is_readable = || {
        let mut poll = libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, no wait.
        unsafe { libc::poll(&mut poll, 1, 0) == 1 }
    };
    assert!(!is_readable());

    mailboxes.queue_a_trigger_fire("go", 1).unwrap();
    assert!(
        is_readable(),
        "with no link and no listener, a fire still wakes"
    );

    mailboxes.drain_listener();
    assert!(!is_readable());
    assert!(
        mailboxes.has_data("go"),
        "a drain clears the wake, never the fire"
    );
}
