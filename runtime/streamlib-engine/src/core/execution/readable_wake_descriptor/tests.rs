// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::ReadableWakeDescriptor;

fn is_readable(wake: &ReadableWakeDescriptor) -> bool {
    use std::os::fd::AsRawFd;
    let mut poll = libc::pollfd {
        fd: wake.readable_end().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd, no wait.
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    ready == 1 && poll.revents & libc::POLLIN != 0
}

#[test]
fn a_wake_stays_readable_however_often_it_is_made_so_until_it_is_drained() {
    let wake = ReadableWakeDescriptor::create().unwrap();
    assert!(!is_readable(&wake));

    for _ in 0..1000 {
        wake.make_readable().unwrap();
    }
    assert!(is_readable(&wake));

    wake.drain();
    assert!(!is_readable(&wake));
    wake.drain();
    assert!(
        !is_readable(&wake),
        "draining a drained wake neither blocks nor wakes"
    );

    wake.make_readable().unwrap();
    assert!(is_readable(&wake));
}
