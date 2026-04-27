// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Two subprocesses simultaneously hold read access on the same surface
//! (each via its own imported VkImage + timeline). Asserts the host
//! grants both reads concurrently and that both subprocesses progress.

#![cfg(target_os = "linux")]

#[path = "common.rs"]
mod common;

use std::sync::Arc;

#[test]
fn two_subprocesses_concurrently_read_same_surface() {
    let host = match common::HostFixture::try_new() {
        Some(h) => h,
        None => {
            println!("concurrent_reads_two_subprocesses: skipping — no Vulkan");
            return;
        }
    };

    let surface = host.register_surface(33, 64, 64);

    // Warm up the surface — host writes once so subprocesses have a
    // known starting layout.
    {
        let _w = host
            .ctx
            .acquire_write(&surface.descriptor)
            .expect("warm-up write");
    }
    assert_eq!(surface.timeline.current_value().unwrap(), 1);

    // Spawn two subprocesses, each waits on value 1 and signals 2.
    let (mut child_a, sock_a) = common::spawn_helper("wait-only");
    let (mut child_b, sock_b) = common::spawn_helper("wait-only");

    // Each subprocess imports its own copy of the timeline and the
    // DMA-BUF — fresh fds per spawn since SCM_RIGHTS dups by the kernel.
    for sock in [&sock_a, &sock_b] {
        let dma_buf_fd = surface
            .texture
            .vulkan_inner()
            .export_dma_buf_fd()
            .expect("export DMA-BUF");
        let sync_fd = Arc::clone(&surface.timeline)
            .export_opaque_fd()
            .expect("export sync_fd");
        let req = common::helper_descriptor("wait-only", &surface, 1, None);
        common::send_helper_request(sock, &req, &[dma_buf_fd], sync_fd)
            .expect("send helper request");
    }

    // Both helpers should respond ok; the timeline signals from both
    // are coalesced (same target value 2 — OPAQUE_FD timeline counter
    // is monotonic so a redundant signal at value 2 is a no-op).
    let resp_a = common::recv_helper_response(&sock_a);
    let resp_b = common::recv_helper_response(&sock_b);
    assert_eq!(resp_a["ok"], true, "child A: {}", resp_a["note"]);
    assert_eq!(resp_b["ok"], true, "child B: {}", resp_b["note"]);

    assert_eq!(child_a.wait().expect("wait a").code(), Some(0));
    assert_eq!(child_b.wait().expect("wait b").code(), Some(0));

    // Test that the host adapter ALSO permits two concurrent reads in
    // process — `run_conformance` already covers this, but exercising
    // it after the cross-process round-trip catches any state-leak that
    // disturbed the in-process counter accounting.
    let r1 = host.ctx.acquire_read(&surface.descriptor).expect("read 1");
    let r2 = host.ctx.acquire_read(&surface.descriptor).expect("read 2");
    drop(r1);
    drop(r2);
}
